use std::collections::HashMap;
use std::ffi::OsStr;
use std::future::Future;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    Request,
};
use libc::ENOENT;
use tracing::{debug, trace};
use wnfs::private::PrivateNode;

use crate::fs::Wnfs;

const TTL: Duration = Duration::from_secs(1); // 1 second
const ROOT_INO: u64 = 1;
const BLOCK_SIZE: usize = 512;

/// Mount a filesystem
pub fn mount(fs: Wnfs, mountpoint: impl AsRef<Path>) -> anyhow::Result<()> {
    let fs = WnfsFuse::new(fs);
    let mountpoint = mountpoint.as_ref().to_owned();
    let options = vec![
        MountOption::RW,
        MountOption::FSName("wnfs".to_string()),
        MountOption::AutoUnmount,
        MountOption::AllowRoot,
    ];
    debug!("mount FUSE at {mountpoint:?}");
    fuser::mount2(fs, mountpoint, &options)?;
    Ok(())
}

/// Inode index for a filesystem.
///
/// This is a partial view of the filesystem and contains only nodes that have been accessed
/// in the current session. Inode numbers are assigned sequentially on first use.
#[derive(Default, Debug)]
pub struct Inodes {
    inodes: HashMap<u64, Inode>,
    by_path: HashMap<Vec<String>, u64>,
    counter: u64,
}

impl Inodes {
    pub fn push(&mut self, path_segments: Vec<String>) -> u64 {
        // pub fn push(&mut self, path_segments: Vec<String>, kind: FileType) -> u64 {
        self.counter += 1;
        let ino = self.counter;
        let inode = Inode::new(ino, path_segments);
        self.by_path.insert(inode.path_segments.clone(), ino);
        self.inodes.insert(ino, inode);
        ino
    }
    pub fn get(&self, ino: u64) -> Option<&Inode> {
        self.inodes.get(&ino)
    }

    pub fn get_path_segments(&self, ino: u64) -> Option<&Vec<String>> {
        self.get(ino).map(|node| &node.path_segments)
    }

    pub fn get_by_path(&self, path: &[String]) -> Option<&Inode> {
        self.by_path.get(path).and_then(|ino| self.inodes.get(ino))
    }

    pub fn get_or_push(&mut self, path: &[String]) -> Inode {
        let path = path.to_vec();
        let id = if let Some(id) = self.by_path.get(&path) {
            *id
        } else {
            self.push(path)
        };
        self.get(id).unwrap().clone()
    }
}

#[derive(Debug, Clone)]
pub struct Inode {
    pub path_segments: Vec<String>,
    pub ino: u64,
}

impl Inode {
    pub fn new(ino: u64, path_segments: Vec<String>) -> Self {
        Self { path_segments, ino }
    }
}

pub struct WnfsFuse {
    pub(crate) wnfs: Wnfs,
    pub(crate) inodes: Inodes,
}

impl WnfsFuse {
    pub fn new(wnfs: Wnfs) -> Self {
        let mut inodes = Inodes::default();
        // Init root inode.
        inodes.push(vec![]);
        Self { wnfs, inodes }
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    futures::executor::block_on(future)
}

impl Filesystem for WnfsFuse {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        trace!("lookup: i{parent} {name:?}");
        let Some(path_segments) = self.inodes.get_path_segments(parent) else {
            trace!("  ENOENT");
            reply.error(ENOENT);
            return;
        };
        let path = push_segment(&path_segments, &name.to_str().unwrap());
        let Inode { ino, .. } = self.inodes.get_or_push(&path);
        match block_on(self.wnfs.get_node(&path)) {
            Ok(Some(node)) => {
                let attr = node_to_attr(ino, &node);
                trace!("  ok {attr:?}");
                reply.entry(&TTL, &attr, 0);
            }
            Ok(None) => {
                trace!("  ENOENT (not found)");
                reply.error(ENOENT);
            }
            Err(err) => {
                trace!("  ENOENT ({err})");
                reply.error(ENOENT);
            }
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        trace!("getattr: i{ino}");

        let node = if ino == ROOT_INO {
            PrivateNode::Dir(self.wnfs.private_root())
        } else {
            let Some(path_segments) = self.inodes.get_path_segments(ino) else {
                trace!("  ENOENT (ino not found)");
                reply.error(ENOENT);
                return;
            };
            let Ok(Some(node)) = block_on(self.wnfs.get_node(&path_segments)) else {
                trace!("  ENOENT (path not found)");
                reply.error(ENOENT);
                return;
            };
            node
        };
        let attr = node_to_attr(ino, &node);
        trace!("  ok {attr:?}");
        reply.attr(&TTL, &attr)
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        trace!("read: i{ino} offset {offset} size {size}");
        let Some(path_segments) = self.inodes.get_path_segments(ino) else {
              trace!("  ENOENT (ino not found)");
              reply.error(ENOENT);
              return;
        };
        let content = block_on(self.wnfs.read_file_at(
            &path_segments,
            offset as usize,
            size as usize,
        ));
        // let content = block_on(self.wnfs.read_file(&path_segments));
        match content {
            Ok(data) => {
                trace!("  ok, len {}", data.len());
                reply.data(&data)
            }
            Err(err) => {
                trace!("  ENOENT ({err})");
                reply.error(ENOENT);
            }
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        trace!("readdir: i{ino} offset {offset}");
        let path_segments = {
            // We're cloning the path segments here to not keep an immutable borrow to self.inodes around.
            // TODO: Maybe always wrap Inode an Rc
            let Some(path_segments) = self.inodes.get_path_segments(ino) else {
                trace!("  ENOENT (ino not found)");
                reply.error(ENOENT);
                return;
            };
            path_segments.to_owned()
        };
        let dir = if path_segments.len() == 0 {
            self.wnfs.private_root()
        } else {
            let Ok(Some(PrivateNode::Dir(dir))) = block_on(self.wnfs.get_node(&path_segments)) else {
                  trace!("  ENOENT (dir not found)");
                  reply.error(ENOENT);
                  return;
            };
            dir
        };

        let mut entries = vec![
            (ino, FileType::Directory, "."),
            (ino, FileType::Directory, ".."),
        ];

        for name in dir.entries() {
            let path = push_segment(&path_segments, name);
            let node = block_on(self.wnfs.get_node(&path));
            match node {
                Ok(Some(node)) => match node {
                    PrivateNode::Dir(_dir) => {
                        let ino = self.inodes.get_or_push(&path);
                        entries.push((ino.ino, FileType::Directory, name));
                    }
                    PrivateNode::File(_file) => {
                        let ino = self.inodes.get_or_push(&path);
                        entries.push((ino.ino, FileType::RegularFile, name));
                    }
                },
                _ => {
                    // todo
                }
            }
        }
        trace!("  ok {entries:?}");

        for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
            // i + 1 means the index of the next entry
            if reply.add(entry.0, (i + 1) as i64, entry.1, entry.2) {
                break;
            }
        }
        reply.ok();
    }

    // fn open(&mut self, _req: &Request<'_>, _ino: u64, _flags: i32, reply: fuser::ReplyOpen) {
    // }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        trace!("mkdir : i{parent} {name:?}");
        let Some(path_segments) = self.inodes.get_path_segments(parent) else {
            trace!("  ENOENT: parent not found");
            reply.error(ENOENT);
            return;
        };
        let path = push_segment(path_segments, name.to_string_lossy());
        match block_on(self.wnfs.mkdir(&path)) {
            Ok(_) => match block_on(self.wnfs.get_node(&path_segments)) {
                Ok(Some(node)) => {
                    let ino = self.inodes.get_or_push(&path);
                    let attr = node_to_attr(ino.ino, &node);
                    trace!("  ok, created! ino {}", ino.ino);
                    reply.entry(&TTL, &attr, 0);
                }
                Err(_) | Ok(None) => {
                    trace!("  ENOENT, failed to find created dir");
                    reply.error(ENOENT);
                }
            },
            Err(err) => {
                trace!("  ENOENT, failed to create dir: {err}");
                reply.error(ENOENT);
            }
        }
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        let size = data.len();
        trace!("write i{ino} offset {offset} size {size}");
        reply.error(ENOENT);
    }
}

fn node_to_attr(ino: u64, node: &PrivateNode) -> FileAttr {
    let metadata = match node {
        PrivateNode::File(file) => file.get_metadata(),
        PrivateNode::Dir(dir) => dir.get_metadata(),
    };
    let kind = match node {
        PrivateNode::File(_) => FileType::RegularFile,
        PrivateNode::Dir(_) => FileType::Directory,
    };
    let perm = match node {
        PrivateNode::File(_) => 0o444,
        PrivateNode::Dir(_) => 0o555,
    };
    let size = match node {
        PrivateNode::File(file) => file.get_content_size_upper_bound(),
        PrivateNode::Dir(_) => 0,
    };
    let nlink = match node {
        PrivateNode::File(_) => 1,
        PrivateNode::Dir(_) => 2,
    };
    let blocks = size / BLOCK_SIZE;
    let mtime = metadata
        .get_modified()
        .map(|x| x.into())
        .unwrap_or(UNIX_EPOCH);
    let ctime = metadata
        .get_created()
        .map(|x| x.into())
        .unwrap_or(UNIX_EPOCH);
    FileAttr {
        ino,
        size: size as u64,
        blocks: blocks as u64,
        nlink,
        perm,
        uid: 1000,
        gid: 1000,
        rdev: 0,
        flags: 0,
        blksize: BLOCK_SIZE as u32,
        kind,
        atime: mtime,
        mtime,
        ctime,
        crtime: ctime,
    }
}

fn push_segment(path_segments: &Vec<String>, name: impl ToString) -> Vec<String> {
    let mut path = path_segments.clone();
    path.push(name.to_string());
    path
}

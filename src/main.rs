//! This example shows how to add a directory to a private forest (a HAMT) where encrypted ciphertexts are stored.
//! It also shows how to retrieve encrypted nodes from the forest using `PrivateRef`s.

use clap::{Parser, Subcommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wnfs_experiments::{fs::Wnfs, fuse};

#[derive(Debug, Parser)]
pub struct Args {
    /// Path to SQLite block store
    #[clap(short, long, default_value = "blocks.db")]
    db_path: String,
    /// Local name (alias) of the private root directory
    #[clap(short, long, default_value = "demo")]
    fs_name: String,
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a directory
    Mkdir {
        path: String,
    },
    /// Print a file to STDOUT
    Cat {
        path: String,
    },
    /// Write STDIN into a file at a path
    Write {
        path: String,
    },
    /// Mount the filesystem with FUSE
    Mount {
        mountpoint: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let mut fs = Wnfs::open_from_path(args.db_path, args.fs_name).await?;

    match args.command {
        Command::Mkdir { path } => {
            let path_segments = into_segments(path);
            fs.mkdir(&path_segments).await?;
        }
        Command::Write { path } => {
            let path_segments = into_segments(path);
            let mut buf = Vec::new();
            let _len = tokio::io::stdin().read_to_end(&mut buf).await?;
            fs.write_file(&path_segments, buf).await?;
        }
        Command::Cat { path } => {
            let path_segments = into_segments(path);
            let buf = fs.read_file(&path_segments).await?;
            tokio::io::stdout().write_all(&buf).await?;
        }
        Command::Mount { mountpoint } => {
            fuse::mount(fs, mountpoint)?;
            // tokio::task::spawn_blocking(|| {
            // });
        }
    }
    Ok(())
}

fn into_segments(path: String) -> Vec<String> {
    path.split("/").map(|x| x.to_owned()).collect()
}

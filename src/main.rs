//! This example shows how to add a directory to a private forest (a HAMT) where encrypted ciphertexts are stored.
//! It also shows how to retrieve encrypted nodes from the forest using `PrivateRef`s.

use clap::{Parser, Subcommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wnfs_experiments::{fs::Fs, fuse};

#[derive(Debug, Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, Subcommand)]
pub enum Command {
    Mkdir { path: String },
    Cat { path: String },
    Write { path: String },
    Mount { mountpoint: String }
}

fn into_segments(path: String) -> Vec<String> {
    path.split("/").map(|x| x.to_owned()).collect()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    let args = Args::parse();
    // Create an in-memory block store.
    // let store = &mut MemoryBlockStore::default();
    let db_path = "blocks.db";
    let fs_name = "demo".to_string();
    let mut fs = Fs::open_from_path(db_path, fs_name).await?;

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
            fuse::mount(mountpoint)?;
        }
    }
    Ok(())
}

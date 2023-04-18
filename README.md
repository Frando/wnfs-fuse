# wnfs-fuse

FUSE filesystem for WNFS

## Usage

See
```
cargo run -- --help

```

E.g.
```
echo "hello world" | cargo run --release -- write hello.txt
mkdir /tmp/mnt
cargo run --release -- mount /tmp/mnt
cat /tmp/mnt/hello.txt
```

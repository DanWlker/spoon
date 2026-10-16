# Installation

### Homebrew

```
$ brew tap DanWlker/spoon
$ brew install spoon
```

### Cargo

```
$ cargo install --locked --git https://github.com/DanWlker/spoon
```

### Build from source

```
$ git clone https://github.com/DanWlker/spoon.git
$ cd spoon
$ cargo build --release # Unless it's a release build, it's very slow.
$ ./target/release/spoon
```

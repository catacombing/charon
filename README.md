# Charon

<p>
  <img src="./logo.svg" width="10%" align="left">

  Charon is a mobile-optimized Wayland maps and navigation application.

  <br clear="align"/>
</p>

<br />

## Screenshots

<p align="center">
  <img src="https://github.com/user-attachments/assets/1f663644-839c-47b6-ad86-d33f0f4b53f2" width="30%"/>
  <img src="https://github.com/user-attachments/assets/e81b74fe-bb86-40b8-bdbc-a5d75f38c7d5" width="30%"/>
</p>

## Building from Source

Charon is compiled with cargo, which creates a binary at `target/release/charon`:

```sh
cargo build --release
```

To compile Charon, the following dependencies are required:
 - boost (compile time)
 - kyotocabinet (runtime)
 - sqlite3 (runtime)
 - marisa (runtime)

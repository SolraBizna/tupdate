TUpdate is an updater written in Rust. It serves a pretty niche purpose. It might be useful for things like video game modpacks shared among a small group of friends. It's probably not in a state that can be relied upon yet.

# Requirements

Client machines must be able to run Rust programs. The server can run any HTTP server capable of serving files.

# Usage

You will need to create an `index.lua` file on the server, as well as `.cat` files describing all downloadable files, and the downloadable files themselves. Then you can run `tupdate` on the clients, either with `URL=http://<your server>/<path to index.lua>` in a file `tupdate.conf` in the same directory as the executable, or with the URL passed directly on the command line.

# TODO

- Explain what `index.lua` looks like
- Explain cat files, and make a tool that makes them
- GUI frontends
    - Cocoa
    - GTK+
    - Win32
- Testing, testing, and more testing
- Polish
- Translations (possibly including Polish)

# Legalese

TUpdate is copyright 2023, Solra Bizna, and licensed under either of:

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or
   <http://www.apache.org/licenses/LICENSE-2.0>)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the TUpdate crate by you, as defined
in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.

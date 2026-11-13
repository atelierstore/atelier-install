# Page test

A test for Atelier's installer, to run in [rootshell](https://apps.apple.com/app/rootshell-local-terminal-ssh/id6755794662) on an iPhone. It isn't part of the installer, and nothing here is built into it.

The installer asks its questions in rootshell's terminal. This tries another way: the program serves a page of its own, which a Shortcut's "Show Web Page" action or Safari shows, and the page asks and answers. The test measures the two things that has to rest on:

- a program rootshell runs can serve a page and take what's typed on it;
- rootshell goes on working at speed while that page is in front of it.

## Running it

In rootshell:

```sh
true && curl -fsSLO https://raw.githubusercontent.com/atelierstore/atelier-install/rootshell-page-test/spike/page-test/atelier-page-test.wasm && ./atelier-page-test.wasm
```

Then open <http://127.0.0.1:8626/> on the same iPhone: in Safari, or with a Shortcut's "Show Web Page" action.

The page shows rootshell's speed second by second, as a share of its best second, the longest pause, the share of its time the program waits for rootshell to answer its calls, and how many connections came and said nothing. Its form sends two words to the program, which says how long each is and keeps neither. The terminal says the same every 5 seconds, so what happened while rootshell was behind another app can be read there afterwards.

The test ends by itself: with the page's Stop button, 30 seconds after the page closes, or after 10 minutes. `--minutes 3` changes that, and `--port 8627` the port.

## What it rests on in rootshell

Read in rootshell's source (92ded20). None of it is confirmed on an iPhone yet.

- **Its calls all wait.** `accept` and `recv` return when a connection or its bytes come, with no time limit and no poll. So the program can't look at its port and work in turns the usual way. Here it connects to its own port before each `accept` and sends a word nobody else knows, a knock: `accept` then returns at once, with a browser's connection if one came first (rootshell hands them over in the order they came) or with the program's own, which says nobody was waiting.
- **A connection that says nothing holds everything up.** The first read of a browser's connection waits until the browser sends its request or closes. The test counts those and how long each took.
- **A listener is on every address the iPhone has**, and a program can't ask who connected, so a page with anything private on it needs a key in its address. This test has nothing private, so it has none. It answers only requests for `127.0.0.1` or `localhost`, and a POST only from its own page.
- **`listen` answers before the listener is ready, and says nothing if the port can't be had.** The test waits a moment, then finds out by connecting.
- **rootshell doesn't close a port when its program ends without closing it**, as one stopped with Control-C does. The listener stays, and takes the port's connections and drops them, until rootshell is quit. So the test closes its port itself on every way out but Control-C, and says the port is taken when its own connections to it are dropped.
- **rootshell keeps what a stopped program was waiting for**, an `accept` or a `recv`, under the socket's number, and the next program in the same tab gets the same numbers. The kept `accept` is given the next listener's first connection, and the kept `recv` the first bytes of the next connection with that number, and both are lost. So the test knocks four times before its first `accept` and numbers its knocks. Against a kept `recv` it can do nothing: a new tab starts clean.

## Building and trying it on a computer

```sh
cargo test
cargo build --release --target wasm32-wasip1
python3 ../../harness/rootshell.py --dir . target/wasm32-wasip1/release/atelier-page-test.wasm
```

`harness/rootshell.py` answers rootshell's calls with the system's sockets. `cargo run` works too, with the system's sockets directly.

`atelier-page-test.wasm` in this folder is the release build, kept here so rootshell can download it from the branch.

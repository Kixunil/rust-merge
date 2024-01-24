# A practical Rust merge tool (PoC)

**This is a proof-of-concept!** It somewhat works but you can't blindly rely on it!

Rust-merge aims to provide a merge tool that understands Rust code and can resolve conflicts based on that knowledge.
However the order of implementing new merge strategies focuses on best cost/benefit ratio first.
While it'd be nice to have a super-powerful merge tool eventually, the author doesn't have the time to do that.
Feel free to contribute or fork it!

## Current status

Currently `rust-merge` can only handle merging of `use` items and even that has limitations (known and unknown).
This was deemed to be the most practical starting point because:

* `use` items are pretty easy to merge, it only matters that the code which needs an item has it
* `use` items are generally put near each-other even when they are unrelated, this is often leads to conflicts
* it is often the case that a conflict caused by `use` statements is the only one in the file

Some important limitations:

* **Run it from top-level directory!** (this is some git weirdness)
* only works on valid Rust files (enforcing error-free `cargo check` on each commit should be enough)
* requires `rustfmt` (`nightly` is hardcoded now) and `diff3` in `$PATH`
* will change formatting in some cases (`::{self, Foo}` may put the module at different line)
* Different visibility or attributes of the same item are considered conflicting, this shouldn't be hard to change.
* Some things are less tested there could be buggy edge cases. You're testing your merged code, right?
* The code is not great/clever. To merge the `use` items a bit convoluted trick is used (see below).
* the lines between `use` items are moved after them, including empty lines
* comments around `use` items are probably lost or moved (didnt' test)
* fails if any of the files has no `use` items
* only merges `use` items in outermost module of a file, not in submodules/functions
* if you rename a file in a way that `git` doesn't see as renamed all hell breaks loose, maybe even if you rename at all, I didn't actually try

## Usage

0. Compile & install `rust-merge`
1. configure git `mergetool.rust-merge.path = /path/to/rust-merge` and `cmd = /path/to/rust-merge \"$BASE\" \"$LOCAL\" \"$REMOTE\" \"$MERGED\"`
2. When you experience a conflict in `.rs` file run `git mergetool -t rust-merge your/conflicting/file` from **root of git repository**
3. Do **not** trust exit code or blindly confirm merge success - check it manually afterwards!

## Future plans

I'd like to take a look into these things eventually.
If you can't wait send a PR!

* `Cargo.toml`/`Cargo.lock` dependencies
* functions/structs/traits/... added next to each-other
* indentation changes
* better `git` integration - the current usage seems weird/crappy, I'm probably misunderstanding something but this part of git isn't really documented well

## How `use` merging works internally

0. The AST of each file is parsed using `syn`
1. The `use` items in top-level module are diffed - base against local and base against remote
2. The recoreded changes are merged - removals use simple union, added items that resolve to the same name with different paths, attributes, or visibility are considered conflict
3. The merged changes are written out into a temp file which is formatted
4. The formatted result is injected to each file at the position of the first `use` item, and all other `use` items are deleted (this is in temporary files)
5. The three temporary files have exactly same `use` section in top-level module and are compared using diff3, the result is written into `$MERGED` file

## License

MIT

Plus I kindly ask you to provide Linux support if you make this into a commercial product.
I'd prefer paying for this as a product but there was nothing available...

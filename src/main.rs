use std::{fmt, io};
use std::collections::{HashSet, HashMap};
use std::path::{Path, PathBuf};

fn main() {
    let mut args = std::env::args_os();
    args.next().expect("missing arg");

    let base = PathBuf::from(args.next().expect("missing arg"));
    let a = args.next().expect("missing arg");
    let b = args.next().expect("missing arg");
    let base = PathBuf::from(base);
    let out = args.next();

    let base = std::fs::read_to_string(base).unwrap();
    let a = std::fs::read_to_string(a).unwrap();
    let b = std::fs::read_to_string(b).unwrap();
    let conflict_free = match out {
        Some(outfile) => {
            let mut output = std::fs::File::create(outfile).expect("failed to open output");
            merge(&base, &a, &b, &mut output).unwrap()
        },
        None => {
            merge(&base, &a, &b, &mut io::stdout().lock()).unwrap()
        },
    };
    if !conflict_free {
        // It looks like git expects all-or-nothing - if the file is only partially merged it'd
        // still revert it. If anyone knows how to fix it I'll be happy to get a hint/PR.
        //std::process::exit(1);
    }
}

fn merge(base: &str, a: &str, b: &str, output: &mut impl io::Write) -> io::Result<bool> {
    use io::{Read, Write, Seek};

    let base_ast: syn::File = syn::parse_str(&base).unwrap(); 
    let a_ast: syn::File = syn::parse_str(&a).unwrap(); 
    let b_ast: syn::File = syn::parse_str(&b).unwrap(); 

    let a_diff = diff_use_items(&base_ast.items, &a_ast.items);
    let b_diff = diff_use_items(&base_ast.items, &b_ast.items);
    let mut conflicts = Vec::new();
    let mut globs = HashMap::new();
    for item in &a_diff.added {
        if let Some(other) = b_diff.added.get(item) {
            if !other.is_same(item) {
                conflicts.push((item, other));
            }
        }
        if let ProducedItem::Glob(path, vis, attr) = item {
            globs.insert(path.clone(), (vis.clone(), attr.to_owned()));
        }
    }
    for item in &b_diff.added {
        // TODO: check vis conflicts
        if let ProducedItem::Glob(path, vis, attr) = item {
            globs.insert(path.clone(), (vis.clone(), attr.to_owned()));
        }
    }

    if !conflicts.is_empty() {
        eprintln!("conflicts:");
        for (a, b) in conflicts {
            eprintln!(" * A added `{};`, b added `{}`", a, b);
        }
        std::process::exit(1);
    }
    let mut base_uses = base_set(&base_ast.items);
    for removed in a_diff.removed {
        base_uses.remove(&removed);
    }
    for removed in b_diff.removed {
        base_uses.remove(&removed);
    }

    for added in a_diff.added {
        match added {
            ProducedItem::Single(_, path, vis, attr) if globs.contains_key(&path) => (),
            _ => { base_uses.insert(added); },
        }
    }
    for added in b_diff.added {
        match added {
            ProducedItem::Single(_, path, vis, attr) if globs.contains_key(&path) => (),
            _ => { base_uses.insert(added); },
        }
    }

    let temp = mktemp::Temp::new_file()?;
    let temp_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .open(&temp)
        .expect("failed to open temp file");
    let mut temp_file = io::BufWriter::new(temp_file);

    for item in base_uses {
        writeln!(temp_file, "{}", item)?;
    }

    let mut temp_file = temp_file.into_inner()?;

    let mut rustfmt = std::process::Command::new("rustfmt");
    if Path::new("rustfmt.toml").try_exists()? {
        rustfmt
            // TODO: make it configurable
            .arg("+nightly")
            .arg("--config-path")
            .arg("rustfmt.toml");
    }

    let status = rustfmt
        .arg(&*temp)
        .stderr(std::process::Stdio::inherit())
        .spawn()?
        .wait()?;
    if !status.success() {
        panic!("rustfmt failed");
    }
    temp_file.seek(io::SeekFrom::Start(0))?;
    let mut use_items = String::new();
    temp_file.read_to_string(&mut use_items).unwrap();

    // scope because of temp files (notice std::process::exit below)
    let status = {
        let base_temp = mktemp::Temp::new_file()?;
        let a_temp = mktemp::Temp::new_file()?;
        let b_temp = mktemp::Temp::new_file()?;
        // scope because of file closes
        {
            use std::fs;

            let mut base_temp_file = io::BufWriter::new(fs::File::create(&*base_temp)?);
            inject_use_items(&use_items, &mut base_temp_file, base, &base_ast.items)?;
            base_temp_file.flush()?;
            let mut a_temp_file = io::BufWriter::new(fs::File::create(&*a_temp)?);
            inject_use_items(&use_items, &mut a_temp_file, a, &a_ast.items)?;
            a_temp_file.flush()?;
            let mut b_temp_file = io::BufWriter::new(fs::File::create(&*b_temp)?);
            inject_use_items(&use_items, &mut b_temp_file, b, &b_ast.items)?;
            b_temp_file.flush()?;
        }
        let mut child = std::process::Command::new("diff3")
            .stdout(std::process::Stdio::piped())
            .env_remove("MERGE")
            .env_remove("LOCAL")
            .env_remove("REMOTE")
            .env_remove("BASE")
            .arg("-m")
            .arg(&*a_temp)
            .arg(&*base_temp)
            .arg(&*b_temp)
            .spawn()?;
        let mut stdout = child.stdout.take().expect("std has ugly command API");
        io::copy(&mut stdout, output)?;
        output.flush()?;
        let status = child.wait()?;
        status
    };
    if !status.success() {
        eprintln!("diff3 failed {}", status);
        Ok(false)
    } else { 
        Ok(true)
    }
}

fn inject_use_items(use_items: &str, target: &mut impl io::Write, source: &str, ast: &[syn::Item]) -> io::Result<()> {
    use proc_macro2::LineColumn;
    fn next_use(ast_iter: &mut core::slice::Iter<'_, syn::Item>) -> Option<(LineColumn, LineColumn)> {
        ast_iter.by_ref().find_map(|item| {
            use syn::Item::*;
            match item {
                Use(item) => {
                    let start = match item.attrs.first() {
                        Some(attr) => attr.pound_token.spans[0].start(),
                        None => {
                            match &item.vis {
                                syn::Visibility::Public(pub_token) => pub_token.span.start(),
                                syn::Visibility::Restricted(restricted) => restricted.pub_token.span.start(),
                                syn::Visibility::Inherited => item.use_token.span.start(),
                            }
                        }
                    };
                    Some((start, item.semi_token.span.end()))
                },
                _ => None,
            }
        })
    }
    let mut ast_iter = ast.iter();
    let span = next_use(&mut ast_iter);
    let (start, end) = span.expect("injecting `use` into scopes without `use` is unimplemented");
    let mut lines = source.split_inclusive('\n');
    let mut line = lines.next().unwrap();
    let mut cur_line = 1; // lines are 1-indexed (:throwing_up:)
    while cur_line < start.line {
        target.write_all(line.as_bytes())?;
        line = lines.next().unwrap();
        cur_line += 1;
    }
    let indent = &line[..start.column];
    assert!(indent.chars().all(|c| c.is_whitespace()), "use line begins with non-whitespace");
    for use_line in use_items.split_inclusive('\n') {
        target.write_all(indent.as_bytes())?;
        target.write_all(use_line.as_bytes())?;
    }
    while cur_line < end.line {
        lines.next().unwrap();
        cur_line += 1;
    }
    let mut next_use_item = next_use(&mut ast_iter);

    for line in lines {
        cur_line += 1;
        if let Some((start, end)) = next_use_item {
            if cur_line >= end.line {
                next_use_item = next_use(&mut ast_iter);
            }
            if cur_line >= start.line {
                continue;
            }
        }
        target.write_all(line.as_bytes())?;
    }

    Ok(())
}

fn base_set(base: &[syn::Item]) -> HashSet<ProducedItem<'_>> {
    let mut base_items = HashSet::new();
    for item in base {
        match item {
            syn::Item::Use(item) => {
                produced_names(&item, |produced| {
                    if let Some(prev) = base_items.replace(produced) {
                        panic!("broken input file - duplicate item: {}", prev);
                    };
                });
            },
            _ => (),
        }
    }
    base_items
}

fn diff_use_items<'a>(base: &'a [syn::Item], new: &'a [syn::Item]) -> Diff<'a> {
    let mut base_items = base_set(base);
    let mut added = HashSet::new();
    let mut removed = HashSet::new();
    for item in new {
        match item {
            syn::Item::Use(item) => {
                produced_names(&item, |produced| {
                    match base_items.take(&produced) {
                        Some(original) if original.is_same(&produced) => (), // stayed the same
                        Some(original) => {
                            removed.insert(original);
                            added.insert(produced);
                        },
                        None => { added.insert(produced); },
                    }
                });
            },
            _ => (),
        }
    }
    removed.extend(base_items.into_iter());
    Diff {
        removed,
        added,
    }
}

fn produced_names<'a>(item: &'a syn::ItemUse, mut cb: impl FnMut(ProducedItem<'a>)) {
    produced_names_inner(&item.tree, &mut Vec::new(), &mut cb, &item.vis, &item.attrs)
}

fn produced_names_inner<'a>(item: &'a syn::UseTree, cur_path: &mut Vec<&'a syn::Ident>, cb: &mut impl FnMut(ProducedItem<'a>), vis: &'a syn::Visibility, attrs: &'a [syn::Attribute]) {
    let item = match item {
        syn::UseTree::Path(path) => {
            cur_path.push(&path.ident);
            produced_names_inner(&path.tree, cur_path, cb, vis, attrs);
            cur_path.pop();
            return
        },
        syn::UseTree::Name(name) if name.ident == "self" => {
            ProducedItem::Single(&cur_path.last().unwrap(), cur_path[..(cur_path.len() - 1)].to_owned(), vis, attrs)
        },
        syn::UseTree::Name(name) => ProducedItem::Single(&name.ident, cur_path.clone(), vis, attrs),
        syn::UseTree::Rename(rename) => ProducedItem::Rename(&rename.rename, &rename.ident, cur_path.clone(), vis, attrs),
        syn::UseTree::Glob(_) => ProducedItem::Glob(cur_path.clone(), vis, attrs),
        syn::UseTree::Group(group) => {
            for item in group.items.iter() {
                produced_names_inner(&item, cur_path, cb, vis, attrs);
            }
            return
        },
    };
    cb(item)
}

#[derive(Debug, Eq)]
enum ProducedItem<'a> {
    Single(&'a syn::Ident, Vec<&'a syn::Ident>, &'a syn::Visibility, &'a [syn::Attribute]),
    Rename(&'a syn::Ident, &'a syn::Ident, Vec<&'a syn::Ident>, &'a syn::Visibility, &'a [syn::Attribute]),
    Glob(Vec<&'a syn::Ident>, &'a syn::Visibility, &'a [syn::Attribute])
}

impl<'a> ProducedItem<'a> {
    fn is_same(&self, other: &Self) -> bool {
        use ProducedItem::*;

        match (self, other) {
            (Single(name1, path1, vis1, attr1), Single(name2, path2, vis2, attr2)) => name1 == name2 && path1 == path2 && vis1 == vis2 && attr1 == attr2,
            (Rename(name1, orig_name1, path1, vis1, attr1), Rename(name2, orig_name2, path2, vis2, attr2)) => name1 == name2 && path1 == path2 && orig_name1 == orig_name2 && vis1 == vis2 && attr1 == attr2,
            (Glob(path1, vis1, attr1), Glob(path2, vis2, attr2)) => path1 == path2 && vis1 == vis2 && attr1 == attr2,
            _ => false,
        }
    }
}

impl<'a> fmt::Display for ProducedItem<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fn display_path(path: &[&syn::Ident], f: &mut fmt::Formatter) -> fmt::Result {
            for item in path {
                write!(f, "{}::", item)?;
            }
            Ok(())
        }

        fn display_vis(vis: &syn::Visibility, f: &mut fmt::Formatter) -> fmt::Result {
            match vis {
                syn::Visibility::Public(_) => write!(f, "pub use "),
                syn::Visibility::Inherited => write!(f, "use "),
                syn::Visibility::Restricted(_) => todo!(),
            }
        }

        match self {
            Self::Single(name, path, vis, attr) => {
                display_vis(vis, f)?;
                display_path(path, f)?;
                write!(f, "{};", name)
            },
            Self::Rename(name, orig_name, path, vis, attr) => {
                display_vis(vis, f)?;
                display_path(path, f)?;
                write!(f, "{} as {};", orig_name, name)
            },
            Self::Glob(path, vis, attr) => {
                display_vis(vis, f)?;
                display_path(path, f)?;
                write!(f, "*;")
            },
        }
    }
}

impl<'a> PartialEq for ProducedItem<'a> {
    fn eq(&self, other: &ProducedItem<'a>) -> bool {
        use ProducedItem::*;

        match (self, other) {
            (Single(name1, _, _, _), Single(name2, _, _, _)) | (Rename(name1, _, _, _, _), Rename(name2, _, _, _, _)) | (Single(name1, _, _, _), Rename(name2, _, _, _, _)) | (Rename(name1, _, _, _, _), Single(name2, _, _, _)) => name1 == name2,
            (Glob(path1, _, _), Glob(path2, _, _)) => path1 == path2,
            (Glob(_, _, _), Single(_, _, _, _)) | (Glob(_, _, _), Rename(_, _, _, _, _)) | (Single(_, _, _, _), Glob(_, _, _)) | (Rename(_, _, _, _, _), Glob(_, _, _)) => false,
        }
    }
}

impl<'a> std::hash::Hash for ProducedItem<'a> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            Self::Single(name, _, _, _) => name.hash(state),
            Self::Rename(name, _, _, _, _) => name.hash(state),
            Self::Glob(glob, _, _) => glob.hash(state),
        }
    }
}

#[derive(Debug)]
struct Diff<'a> {
    added: HashSet<ProducedItem<'a>>,
    removed: HashSet<ProducedItem<'a>>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn all() {
        for test in std::fs::read_dir("tests/data").unwrap() {
            let test = test.unwrap();
            let test = test.path();
            let base = std::fs::read_to_string(test.join("base.rs")).unwrap();
            let a = std::fs::read_to_string(test.join("a.rs")).unwrap();
            let b = std::fs::read_to_string(test.join("b.rs")).unwrap();
            let output = mktemp::Temp::new_file().unwrap();
            // We could do this in-memory but seeing the diff is better
            let mut output_file = std::fs::File::create(&*output).unwrap();
            super::merge(&base, &a, &b, &mut output_file).unwrap();
            let status = std::process::Command::new("diff")
                .arg(test.join("merged.rs"))
                .arg(&*output)
                .spawn()
                .unwrap()
                .wait()
                .unwrap();
            assert!(status.success());
        }
    }
}

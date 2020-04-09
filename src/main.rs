use std::env;
use std::io::Write;
use std::process::{exit, Command, Stdio};

use failure::*;
use serde_json::{json, Value};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(
    name = "rs-git-fsmonitor",
    about = "Git fsmonitor hook in Rust\nhttps://git-scm.com/docs/githooks#_fsmonitor_watchman"
)]
struct Opt {
    /// The version of the interface
    version: u8,

    /// Watchman clockspec, it can be epoch second or clock id
    token: String,
}

fn main() {
    let opt = Opt::from_args();

    if opt.version != 2 {
        eprintln!("unsupported version");
        exit(1);
    }

    query_watchman(opt.token).unwrap_or_else(|e| {
        eprintln!("{}", pretty_error(&e));
        exit(1);
    })
}

fn query_watchman(token: String) -> Fallible<()> {
    let git_work_tree = env::current_dir().context("Couldn't get working directory")?;

    let mut watchman = Command::new("watchman")
        .args(&["-j", "--no-pretty"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("Couldn't start watchman")?;

    {
        let watchman_query = get_watchman_query(&git_work_tree, token.clone());

        watchman
            .stdin
            .as_mut()
            .expect("child Watchman process's stdin isn't piped")
            .write_all(watchman_query.to_string().as_bytes())?;
    }

    let output = watchman
        .wait_with_output()
        .context("Failed to wait on watchman query")?
        .stdout;

    let response: Value = serde_json::from_str(
        String::from_utf8(output)
            .context("Watchman didn't return valid JSON")?
            .as_str(),
    )?;

    if let Some(err) = response["error"].as_str() {
        ensure!(
            err.contains("unable to resolve root"),
            "Watchman failed for an unexpected reason {} (token: {})",
            err,
            token
        );
        match add_to_watchman(&git_work_tree) {
            Ok(()) => {}
            Err(e) => bail!(e),
        }
        match get_watchman_clock(&git_work_tree) {
            Ok(clock_id) => {
                print!("{}\0/\0", clock_id);
                return Ok(());
            }
            Err(e) => bail!(e),
        }
    }

    match response["files"].as_array() {
        Some(files) => {
            print!("{}\0", response["clock"].as_str().unwrap_or(""));
            for file in files {
                if let Some(filename) = file.as_str() {
                    print!("{}\0", filename);
                }
            }

            Ok(())
        }
        None => bail!("missing file data"),
    }
}

fn add_to_watchman(worktree: &std::path::Path) -> Fallible<()> {
    eprintln!("Adding {} to Watchman's watch list", worktree.display());

    let watchman = Command::new("watchman")
        .args(&[
            "watch",
            worktree
                .to_str()
                .expect("Working directory isn't valid Unicode"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("Couldn't start watchman watch")?;

    let output = watchman
        .wait_with_output()
        .context("Failed to wait on `watchman watch`")?;
    ensure!(output.status.success(), "`watchman watch` failed");

    // Return the fast "everything is dirty" indication to Git.
    // This makes subsequent queries much faster since Git will pass Watchman
    // a timestamp from _after_ it started.
    // (When Watchman gets a time before its run,
    // it conservatively says everything has changed.)
    print!("/\0");
    Ok(())
}

fn get_watchman_clock(worktree: &std::path::Path) -> Fallible<String> {
    let watchman = Command::new("watchman")
        .args(&[
            "clock",
            worktree
                .to_str()
                .expect("Working directory isn't valid Unicode"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("Couldn't start watchman clock")?;
    let output = watchman
        .wait_with_output()
        .context("Failed to wait on watchman clock")?
        .stdout;

    let response: Value = serde_json::from_str(
        String::from_utf8(output)
            .context("Watchman didn't return valid JSON")?
            .as_str(),
    )?;
    match response["clock"].as_str() {
        Some(clock_id) => Ok(String::from(clock_id)),
        None => bail!("failed to call watchman clock"),
    }
}

fn get_watchman_query(git_work_tree: &std::path::Path, token: String) -> Value {
    // the token following `since` expression can be either epoch second as integer or a clock id as string
    let token_value = if let Some('c') = token.chars().next() {
        Value::from(token)
    } else {
        Value::from(token.parse::<u64>().unwrap_or(0) / 1_000_000_000)
    };
    json!(
        [
            "query",
            git_work_tree,
            {
                "since": token_value,
                "fields": ["name"],
                "expression": [
                    "not", [
                        "dirname", ".git"
                    ]
                ]
            }
        ]
    )
}

// Borrowed lovingly from Burntsushi:
// https://www.reddit.com/r/rust/comments/8fecqy/can_someone_show_an_example_of_failure_crate_usage/dy2u9q6/
// Chains errors into a big string.
fn pretty_error(err: &failure::Error) -> String {
    let mut pretty = err.to_string();
    let mut prev = err.as_fail();
    while let Some(next) = prev.cause() {
        pretty.push_str(":\n");
        pretty.push_str(&next.to_string());
        if let Some(bt) = next.backtrace() {
            let mut bts = bt.to_string();
            // If RUST_BACKTRACE is not set, next.backtrace() gives us
            // Some(bt), but bt.to_string() gives us an empty string.
            // If we push a newline to the return value and nothing else,
            // we get something like:
            // ```
            // Some errror
            // :
            // Its cause
            // ```
            if !bts.is_empty() {
                bts.push_str("\n");
                pretty.push_str(&bts);
            }
        }
        prev = next;
    }
    pretty
}

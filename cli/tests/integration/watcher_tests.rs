// Copyright 2018-2023 the Deno authors. All rights reserved. MIT license.

use flaky_test::flaky_test;
use test_util as util;
use test_util::assert_contains;
use test_util::TempDir;
use tokio::io::AsyncBufReadExt;
use util::DenoChild;

use util::assert_not_contains;

const CLEAR_SCREEN: &str = r#"[2J"#;

/// Logs to stderr every time next_line() is called
struct LoggingLines<R>
where
  R: tokio::io::AsyncBufRead + Unpin,
{
  pub lines: tokio::io::Lines<R>,
  pub stream_name: String,
}

impl<R> LoggingLines<R>
where
  R: tokio::io::AsyncBufRead + Unpin,
{
  pub async fn next_line(&mut self) -> tokio::io::Result<Option<String>> {
    let line = self.lines.next_line().await;
    eprintln!(
      "{}: {}",
      self.stream_name,
      line.as_ref().unwrap().clone().unwrap()
    );
    line
  }
}

// Helper function to skip watcher output that contains "Restarting"
// phrase.
async fn skip_restarting_line<R>(stderr_lines: &mut LoggingLines<R>) -> String
where
  R: tokio::io::AsyncBufRead + Unpin,
{
  loop {
    let msg = next_line(stderr_lines).await.unwrap();
    if !msg.contains("Restarting") {
      return msg;
    }
  }
}

async fn read_all_lints<R>(stderr_lines: &mut LoggingLines<R>) -> String
where
  R: tokio::io::AsyncBufRead + Unpin,
{
  let mut str = String::new();
  while let Some(t) = next_line(stderr_lines).await {
    let t = util::strip_ansi_codes(&t);
    if t.starts_with("Watcher File change detected") {
      continue;
    }
    if t.starts_with("Watcher") {
      break;
    }
    if t.starts_with('(') {
      str.push_str(&t);
      str.push('\n');
    }
  }
  str
}

async fn next_line<R>(lines: &mut LoggingLines<R>) -> Option<String>
where
  R: tokio::io::AsyncBufRead + Unpin,
{
  let timeout = tokio::time::Duration::from_secs(60);

  tokio::time::timeout(timeout, lines.next_line())
    .await
    .unwrap_or_else(|_| {
      panic!(
        "Output did not contain a new line after {} seconds",
        timeout.as_secs()
      )
    })
    .unwrap()
}

/// Returns the matched line or None if there are no more lines in this stream
async fn wait_for<R>(
  condition: impl Fn(&str) -> bool,
  lines: &mut LoggingLines<R>,
) -> Option<String>
where
  R: tokio::io::AsyncBufRead + Unpin,
{
  while let Some(line) = lines.next_line().await.unwrap() {
    if condition(line.as_str()) {
      return Some(line);
    }
  }

  None
}

async fn wait_contains<R>(s: &str, lines: &mut LoggingLines<R>) -> String
where
  R: tokio::io::AsyncBufRead + Unpin,
{
  let timeout = tokio::time::Duration::from_secs(60);

  tokio::time::timeout(timeout, wait_for(|line| line.contains(s), lines))
    .await
    .unwrap_or_else(|_| {
      panic!(
        "Output did not contain \"{}\" after {} seconds",
        s,
        timeout.as_secs()
      )
    })
    .unwrap_or_else(|| panic!("Output ended without containing \"{}\"", s))
}

/// Before test cases touch files, they need to wait for the watcher to be
/// ready. Waiting for subcommand output is insufficient.
/// The file watcher takes a moment to start watching files due to
/// asynchronicity. It is possible for the watched subcommand to finish before
/// any files are being watched.
/// deno must be running with --log-level=debug
/// file_name should be the file name and, optionally, extension. file_name
/// may not be a full path, as it is not portable.
async fn wait_for_watcher<R>(
  file_name: &str,
  stderr_lines: &mut LoggingLines<R>,
) -> String
where
  R: tokio::io::AsyncBufRead + Unpin,
{
  let timeout = tokio::time::Duration::from_secs(60);

  tokio::time::timeout(
    timeout,
    wait_for(
      |line| line.contains("Watching paths") && line.contains(file_name),
      stderr_lines,
    ),
  )
  .await
  .unwrap_or_else(|_| {
    panic!(
      "Watcher did not start for file \"{}\" after {} seconds",
      file_name,
      timeout.as_secs()
    )
  })
  .unwrap_or_else(|| {
    panic!(
      "Output ended without before the watcher started watching file \"{}\"",
      file_name
    )
  })
}

fn check_alive_then_kill(mut child: DenoChild) {
  assert!(child.try_wait().unwrap().is_none());
  child.kill().unwrap();
}

fn child_lines(
  child: &mut std::process::Child,
) -> (
  LoggingLines<tokio::io::BufReader<tokio::process::ChildStdout>>,
  LoggingLines<tokio::io::BufReader<tokio::process::ChildStderr>>,
) {
  let stdout_lines = LoggingLines {
    lines: tokio::io::BufReader::new(
      tokio::process::ChildStdout::from_std(child.stdout.take().unwrap())
        .unwrap(),
    )
    .lines(),
    stream_name: "STDOUT".to_string(),
  };
  let stderr_lines = LoggingLines {
    lines: tokio::io::BufReader::new(
      tokio::process::ChildStderr::from_std(child.stderr.take().unwrap())
        .unwrap(),
    )
    .lines(),
    stream_name: "STDERR".to_string(),
  };
  (stdout_lines, stderr_lines)
}

#[tokio::test]
async fn lint_watch_test() {
  let t = TempDir::new();
  let badly_linted_original =
    util::testdata_path().join("lint/watch/badly_linted.js");
  let badly_linted_output =
    util::testdata_path().join("lint/watch/badly_linted.js.out");
  let badly_linted_fixed1 =
    util::testdata_path().join("lint/watch/badly_linted_fixed1.js");
  let badly_linted_fixed1_output =
    util::testdata_path().join("lint/watch/badly_linted_fixed1.js.out");
  let badly_linted_fixed2 =
    util::testdata_path().join("lint/watch/badly_linted_fixed2.js");
  let badly_linted_fixed2_output =
    util::testdata_path().join("lint/watch/badly_linted_fixed2.js.out");
  let badly_linted = t.path().join("badly_linted.js");

  std::fs::copy(badly_linted_original, &badly_linted).unwrap();

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("lint")
    .arg(&badly_linted)
    .arg("--watch")
    .arg("--unstable")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (_stdout_lines, mut stderr_lines) = child_lines(&mut child);
  let next_line = next_line(&mut stderr_lines).await.unwrap();

  assert_contains!(&next_line, "Lint started");
  let mut output = read_all_lints(&mut stderr_lines).await;
  let expected = std::fs::read_to_string(badly_linted_output).unwrap();
  assert_eq!(output, expected);

  // Change content of the file again to be badly-linted
  std::fs::copy(badly_linted_fixed1, &badly_linted).unwrap();
  std::thread::sleep(std::time::Duration::from_secs(1));

  output = read_all_lints(&mut stderr_lines).await;
  let expected = std::fs::read_to_string(badly_linted_fixed1_output).unwrap();
  assert_eq!(output, expected);

  // Change content of the file again to be badly-linted
  std::fs::copy(badly_linted_fixed2, &badly_linted).unwrap();

  output = read_all_lints(&mut stderr_lines).await;
  let expected = std::fs::read_to_string(badly_linted_fixed2_output).unwrap();
  assert_eq!(output, expected);

  // the watcher process is still alive
  assert!(child.try_wait().unwrap().is_none());

  child.kill().unwrap();
  drop(t);
}

#[tokio::test]
async fn lint_watch_without_args_test() {
  let t = TempDir::new();
  let badly_linted_original =
    util::testdata_path().join("lint/watch/badly_linted.js");
  let badly_linted_output =
    util::testdata_path().join("lint/watch/badly_linted.js.out");
  let badly_linted_fixed1 =
    util::testdata_path().join("lint/watch/badly_linted_fixed1.js");
  let badly_linted_fixed1_output =
    util::testdata_path().join("lint/watch/badly_linted_fixed1.js.out");
  let badly_linted_fixed2 =
    util::testdata_path().join("lint/watch/badly_linted_fixed2.js");
  let badly_linted_fixed2_output =
    util::testdata_path().join("lint/watch/badly_linted_fixed2.js.out");
  let badly_linted = t.path().join("badly_linted.js");

  std::fs::copy(badly_linted_original, &badly_linted).unwrap();

  let mut child = util::deno_cmd()
    .current_dir(t.path())
    .arg("lint")
    .arg("--watch")
    .arg("--unstable")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (_stdout_lines, mut stderr_lines) = child_lines(&mut child);

  let next_line = next_line(&mut stderr_lines).await.unwrap();
  assert_contains!(&next_line, "Lint started");
  let mut output = read_all_lints(&mut stderr_lines).await;
  let expected = std::fs::read_to_string(badly_linted_output).unwrap();
  assert_eq!(output, expected);

  // Change content of the file again to be badly-linted
  std::fs::copy(badly_linted_fixed1, &badly_linted).unwrap();

  output = read_all_lints(&mut stderr_lines).await;
  let expected = std::fs::read_to_string(badly_linted_fixed1_output).unwrap();
  assert_eq!(output, expected);

  // Change content of the file again to be badly-linted
  std::fs::copy(badly_linted_fixed2, &badly_linted).unwrap();
  std::thread::sleep(std::time::Duration::from_secs(1));

  output = read_all_lints(&mut stderr_lines).await;
  let expected = std::fs::read_to_string(badly_linted_fixed2_output).unwrap();
  assert_eq!(output, expected);

  // the watcher process is still alive
  assert!(child.try_wait().unwrap().is_none());

  child.kill().unwrap();
  drop(t);
}

#[tokio::test]
async fn lint_all_files_on_each_change_test() {
  let t = TempDir::new();
  let badly_linted_fixed0 =
    util::testdata_path().join("lint/watch/badly_linted.js");
  let badly_linted_fixed1 =
    util::testdata_path().join("lint/watch/badly_linted_fixed1.js");
  let badly_linted_fixed2 =
    util::testdata_path().join("lint/watch/badly_linted_fixed2.js");

  let badly_linted_1 = t.path().join("badly_linted_1.js");
  let badly_linted_2 = t.path().join("badly_linted_2.js");
  std::fs::copy(badly_linted_fixed0, badly_linted_1).unwrap();
  std::fs::copy(badly_linted_fixed1, &badly_linted_2).unwrap();

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("lint")
    .arg(t.path())
    .arg("--watch")
    .arg("--unstable")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (_stdout_lines, mut stderr_lines) = child_lines(&mut child);

  assert_contains!(
    wait_contains("Checked", &mut stderr_lines).await,
    "Checked 2 files"
  );

  std::fs::copy(badly_linted_fixed2, badly_linted_2).unwrap();

  assert_contains!(
    wait_contains("Checked", &mut stderr_lines).await,
    "Checked 2 files"
  );

  assert!(child.try_wait().unwrap().is_none());

  child.kill().unwrap();
  drop(t);
}

#[tokio::test]
async fn fmt_watch_test() {
  let fmt_testdata_path = util::testdata_path().join("fmt");
  let t = TempDir::new();
  let fixed = fmt_testdata_path.join("badly_formatted_fixed.js");
  let badly_formatted_original = fmt_testdata_path.join("badly_formatted.mjs");
  let badly_formatted = t.path().join("badly_formatted.js");
  std::fs::copy(&badly_formatted_original, &badly_formatted).unwrap();

  let mut child = util::deno_cmd()
    .current_dir(&fmt_testdata_path)
    .arg("fmt")
    .arg(&badly_formatted)
    .arg("--watch")
    .arg("--unstable")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (_stdout_lines, mut stderr_lines) = child_lines(&mut child);

  let next_line = next_line(&mut stderr_lines).await.unwrap();
  assert_contains!(&next_line, "Fmt started");
  assert_contains!(
    skip_restarting_line(&mut stderr_lines).await,
    "badly_formatted.js"
  );
  assert_contains!(
    wait_contains("Checked", &mut stderr_lines).await,
    "Checked 1 file"
  );
  wait_contains("Fmt finished", &mut stderr_lines).await;

  let expected = std::fs::read_to_string(fixed.clone()).unwrap();
  let actual = std::fs::read_to_string(badly_formatted.clone()).unwrap();
  assert_eq!(actual, expected);

  // Change content of the file again to be badly formatted
  std::fs::copy(&badly_formatted_original, &badly_formatted).unwrap();

  assert_contains!(
    skip_restarting_line(&mut stderr_lines).await,
    "badly_formatted.js"
  );
  assert_contains!(
    wait_contains("Checked", &mut stderr_lines).await,
    "Checked 1 file"
  );
  wait_contains("Fmt finished", &mut stderr_lines).await;

  // Check if file has been automatically formatted by watcher
  let expected = std::fs::read_to_string(fixed).unwrap();
  let actual = std::fs::read_to_string(badly_formatted).unwrap();
  assert_eq!(actual, expected);
  check_alive_then_kill(child);
}

#[tokio::test]
async fn fmt_watch_without_args_test() {
  let fmt_testdata_path = util::testdata_path().join("fmt");
  let t = TempDir::new();
  let fixed = fmt_testdata_path.join("badly_formatted_fixed.js");
  let badly_formatted_original = fmt_testdata_path.join("badly_formatted.mjs");
  let badly_formatted = t.path().join("badly_formatted.js");
  std::fs::copy(&badly_formatted_original, &badly_formatted).unwrap();

  let mut child = util::deno_cmd()
    .current_dir(t.path())
    .arg("fmt")
    .arg("--watch")
    .arg("--unstable")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (_stdout_lines, mut stderr_lines) = child_lines(&mut child);

  let next_line = next_line(&mut stderr_lines).await.unwrap();
  assert_contains!(&next_line, "Fmt started");
  assert_contains!(
    skip_restarting_line(&mut stderr_lines).await,
    "badly_formatted.js"
  );
  assert_contains!(
    wait_contains("Checked", &mut stderr_lines).await,
    "Checked 1 file"
  );

  let expected = std::fs::read_to_string(fixed.clone()).unwrap();
  let actual = std::fs::read_to_string(badly_formatted.clone()).unwrap();
  assert_eq!(actual, expected);

  // Change content of the file again to be badly formatted
  std::fs::copy(&badly_formatted_original, &badly_formatted).unwrap();
  assert_contains!(
    skip_restarting_line(&mut stderr_lines).await,
    "badly_formatted.js"
  );
  assert_contains!(
    wait_contains("Checked", &mut stderr_lines).await,
    "Checked 1 file"
  );

  // Check if file has been automatically formatted by watcher
  let expected = std::fs::read_to_string(fixed).unwrap();
  let actual = std::fs::read_to_string(badly_formatted).unwrap();
  assert_eq!(actual, expected);
  check_alive_then_kill(child);
}

#[tokio::test]
async fn fmt_check_all_files_on_each_change_test() {
  let t = TempDir::new();
  let fmt_testdata_path = util::testdata_path().join("fmt");
  let badly_formatted_original = fmt_testdata_path.join("badly_formatted.mjs");
  let badly_formatted_1 = t.path().join("badly_formatted_1.js");
  let badly_formatted_2 = t.path().join("badly_formatted_2.js");
  std::fs::copy(&badly_formatted_original, &badly_formatted_1).unwrap();
  std::fs::copy(&badly_formatted_original, badly_formatted_2).unwrap();

  let mut child = util::deno_cmd()
    .current_dir(&fmt_testdata_path)
    .arg("fmt")
    .arg(t.path())
    .arg("--watch")
    .arg("--check")
    .arg("--unstable")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (_stdout_lines, mut stderr_lines) = child_lines(&mut child);

  assert_contains!(
    wait_contains("error", &mut stderr_lines).await,
    "Found 2 not formatted files in 2 files"
  );

  // Change content of the file again to be badly formatted
  std::fs::copy(&badly_formatted_original, &badly_formatted_1).unwrap();

  assert_contains!(
    wait_contains("error", &mut stderr_lines).await,
    "Found 2 not formatted files in 2 files"
  );

  check_alive_then_kill(child);
}

#[tokio::test]
async fn bundle_js_watch() {
  use std::path::PathBuf;
  // Test strategy extends this of test bundle_js by adding watcher
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.ts");
  file_to_watch.write("console.log('Hello world');");
  assert!(file_to_watch.is_file());
  let t = TempDir::new();
  let bundle = t.path().join("mod6.bundle.js");
  let mut deno = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("bundle")
    .arg(&file_to_watch)
    .arg(&bundle)
    .arg("--watch")
    .arg("--unstable")
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();

  let (_stdout_lines, mut stderr_lines) = child_lines(&mut deno);

  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Warning");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "deno_emit");
  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "Bundle started"
  );
  let line = next_line(&mut stderr_lines).await.unwrap();
  assert_contains!(line, "file_to_watch.ts");
  assert_contains!(line, "Check");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Bundle");
  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "mod6.bundle.js"
  );
  let file = PathBuf::from(&bundle);
  assert!(file.is_file());

  wait_contains("Bundle finished", &mut stderr_lines).await;

  file_to_watch.write("console.log('Hello world2');");

  let line = next_line(&mut stderr_lines).await.unwrap();
  // Should not clear screen, as we are in non-TTY environment
  assert_not_contains!(&line, CLEAR_SCREEN);
  assert_contains!(&line, "File change detected!");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Check");
  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "file_to_watch.ts"
  );
  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "mod6.bundle.js"
  );
  let file = PathBuf::from(&bundle);
  assert!(file.is_file());
  wait_contains("Bundle finished", &mut stderr_lines).await;

  // Confirm that the watcher keeps on working even if the file is updated and has invalid syntax
  file_to_watch.write("syntax error ^^");

  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "File change detected!"
  );
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "error: ");
  wait_contains("Bundle failed", &mut stderr_lines).await;
  check_alive_then_kill(deno);
}

/// Confirm that the watcher continues to work even if module resolution fails at the *first* attempt
#[tokio::test]
async fn bundle_watch_not_exit() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.ts");
  file_to_watch.write("syntax error ^^");
  let target_file = t.path().join("target.js");

  let mut deno = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("bundle")
    .arg(&file_to_watch)
    .arg(&target_file)
    .arg("--watch")
    .arg("--unstable")
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (_stdout_lines, mut stderr_lines) = child_lines(&mut deno);

  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Warning");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "deno_emit");
  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "Bundle started"
  );
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "error:");
  assert_eq!(next_line(&mut stderr_lines).await.unwrap(), "");
  assert_eq!(
    next_line(&mut stderr_lines).await.unwrap(),
    "  syntax error ^^"
  );
  assert_eq!(
    next_line(&mut stderr_lines).await.unwrap(),
    "         ~~~~~"
  );
  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "Bundle failed"
  );
  // the target file hasn't been created yet
  assert!(!target_file.is_file());

  // Make sure the watcher actually restarts and works fine with the proper syntax
  file_to_watch.write("console.log(42);");

  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "File change detected"
  );
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Check");
  let line = next_line(&mut stderr_lines).await.unwrap();
  // Should not clear screen, as we are in non-TTY environment
  assert_not_contains!(&line, CLEAR_SCREEN);
  assert_contains!(line, "file_to_watch.ts");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "target.js");

  wait_contains("Bundle finished", &mut stderr_lines).await;

  // bundled file is created
  assert!(target_file.is_file());
  check_alive_then_kill(deno);
}

#[tokio::test]
async fn run_watch_no_dynamic() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  file_to_watch.write("console.log('Hello world');");

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg("--unstable")
    .arg("-L")
    .arg("debug")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);

  wait_contains("Hello world", &mut stdout_lines).await;
  wait_for_watcher("file_to_watch.js", &mut stderr_lines).await;

  // Change content of the file
  file_to_watch.write("console.log('Hello world2');");

  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains("Hello world2", &mut stdout_lines).await;
  wait_for_watcher("file_to_watch.js", &mut stderr_lines).await;

  // Add dependency
  let another_file = t.path().join("another_file.js");
  another_file.write("export const foo = 0;");
  file_to_watch
    .write("import { foo } from './another_file.js'; console.log(foo);");

  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains("0", &mut stdout_lines).await;
  wait_for_watcher("another_file.js", &mut stderr_lines).await;

  // Confirm that restarting occurs when a new file is updated
  another_file.write("export const foo = 42;");

  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains("42", &mut stdout_lines).await;
  wait_for_watcher("file_to_watch.js", &mut stderr_lines).await;

  // Confirm that the watcher keeps on working even if the file is updated and has invalid syntax
  file_to_watch.write("syntax error ^^");

  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains("error:", &mut stderr_lines).await;
  wait_for_watcher("file_to_watch.js", &mut stderr_lines).await;

  // Then restore the file
  file_to_watch
    .write("import { foo } from './another_file.js'; console.log(foo);");

  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains("42", &mut stdout_lines).await;
  wait_for_watcher("another_file.js", &mut stderr_lines).await;

  // Update the content of the imported file with invalid syntax
  another_file.write("syntax error ^^");

  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains("error:", &mut stderr_lines).await;
  wait_for_watcher("another_file.js", &mut stderr_lines).await;

  // Modify the imported file and make sure that restarting occurs
  another_file.write("export const foo = 'modified!';");

  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains("modified!", &mut stdout_lines).await;
  wait_contains("Watching paths", &mut stderr_lines).await;
  check_alive_then_kill(child);
}

// TODO(bartlomieju): this test became flaky on macOS runner; it is unclear
// if that's because of a bug in code or the runner itself. We should reenable
// it once we upgrade to XL runners for macOS.
#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn run_watch_external_watch_files() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  file_to_watch.write("console.log('Hello world');");

  let external_file_to_watch = t.path().join("external_file_to_watch.txt");
  external_file_to_watch.write("Hello world");

  let mut watch_arg = "--watch=".to_owned();
  let external_file_to_watch_str = external_file_to_watch.to_string();
  watch_arg.push_str(&external_file_to_watch_str);

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg(watch_arg)
    .arg("-L")
    .arg("debug")
    .arg("--unstable")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);
  wait_contains("Process started", &mut stderr_lines).await;
  wait_contains("Hello world", &mut stdout_lines).await;
  wait_for_watcher("external_file_to_watch.txt", &mut stderr_lines).await;

  // Change content of the external file
  external_file_to_watch.write("Hello world2");
  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains("Process finished", &mut stderr_lines).await;

  // Again (https://github.com/denoland/deno/issues/17584)
  external_file_to_watch.write("Hello world3");
  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains("Process finished", &mut stderr_lines).await;

  check_alive_then_kill(child);
}

#[tokio::test]
async fn run_watch_load_unload_events() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  file_to_watch.write(
    r#"
      setInterval(() => {}, 0);
      window.addEventListener("load", () => {
        console.log("load");
      });

      window.addEventListener("unload", () => {
        console.log("unload");
      });
    "#,
  );

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg("--unstable")
    .arg("-L")
    .arg("debug")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);

  // Wait for the first load event to fire
  wait_contains("load", &mut stdout_lines).await;
  wait_for_watcher("file_to_watch.js", &mut stderr_lines).await;

  // Change content of the file, this time without an interval to keep it alive.
  file_to_watch.write(
    r#"
      window.addEventListener("load", () => {
        console.log("load");
      });

      window.addEventListener("unload", () => {
        console.log("unload");
      });
    "#,
  );

  // Wait for the restart
  wait_contains("Restarting", &mut stderr_lines).await;

  // Confirm that the unload event was dispatched from the first run
  wait_contains("unload", &mut stdout_lines).await;

  // Followed by the load event of the second run
  wait_contains("load", &mut stdout_lines).await;

  // Which is then unloaded as there is nothing keeping it alive.
  wait_contains("unload", &mut stdout_lines).await;
  check_alive_then_kill(child);
}

/// Confirm that the watcher continues to work even if module resolution fails at the *first* attempt
#[tokio::test]
async fn run_watch_not_exit() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  file_to_watch.write("syntax error ^^");

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg("--unstable")
    .arg("-L")
    .arg("debug")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);

  wait_contains("Process started", &mut stderr_lines).await;
  wait_contains("error:", &mut stderr_lines).await;
  wait_for_watcher("file_to_watch.js", &mut stderr_lines).await;

  // Make sure the watcher actually restarts and works fine with the proper syntax
  file_to_watch.write("console.log(42);");

  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains("42", &mut stdout_lines).await;
  wait_contains("Process finished", &mut stderr_lines).await;
  check_alive_then_kill(child);
}

#[tokio::test]
async fn run_watch_with_import_map_and_relative_paths() {
  fn create_relative_tmp_file(
    directory: &TempDir,
    filename: &'static str,
    filecontent: &'static str,
  ) -> std::path::PathBuf {
    let absolute_path = directory.path().join(filename);
    absolute_path.write(filecontent);
    let relative_path = absolute_path
      .as_path()
      .strip_prefix(directory.path())
      .unwrap()
      .to_owned();
    assert!(relative_path.is_relative());
    relative_path
  }

  let temp_directory = TempDir::new();
  let file_to_watch = create_relative_tmp_file(
    &temp_directory,
    "file_to_watch.js",
    "console.log('Hello world');",
  );
  let import_map_path = create_relative_tmp_file(
    &temp_directory,
    "import_map.json",
    "{\"imports\": {}}",
  );

  let mut child = util::deno_cmd()
    .current_dir(temp_directory.path())
    .arg("run")
    .arg("--unstable")
    .arg("--watch")
    .arg("--import-map")
    .arg(&import_map_path)
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);
  let line = next_line(&mut stderr_lines).await.unwrap();
  assert_contains!(&line, "Process started");
  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "Process finished"
  );
  assert_contains!(next_line(&mut stdout_lines).await.unwrap(), "Hello world");

  check_alive_then_kill(child);
}

#[tokio::test]
async fn run_watch_with_ext_flag() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch");
  file_to_watch.write("interface I{}; console.log(42);");

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg("--log-level")
    .arg("debug")
    .arg("--ext")
    .arg("ts")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);

  wait_contains("42", &mut stdout_lines).await;

  // Make sure the watcher actually restarts and works fine with the proper language
  wait_for_watcher("file_to_watch", &mut stderr_lines).await;
  wait_contains("Process finished", &mut stderr_lines).await;

  file_to_watch.write("type Bear = 'polar' | 'grizzly'; console.log(123);");

  wait_contains("Restarting!", &mut stderr_lines).await;
  wait_contains("123", &mut stdout_lines).await;
  wait_contains("Process finished", &mut stderr_lines).await;

  check_alive_then_kill(child);
}

#[tokio::test]
async fn run_watch_error_messages() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  file_to_watch
    .write("throw SyntaxError(`outer`, {cause: TypeError(`inner`)})");

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (_, mut stderr_lines) = child_lines(&mut child);

  wait_contains("Process started", &mut stderr_lines).await;
  wait_contains("error: Uncaught SyntaxError: outer", &mut stderr_lines).await;
  wait_contains("Caused by: TypeError: inner", &mut stderr_lines).await;
  wait_contains("Process failed", &mut stderr_lines).await;

  check_alive_then_kill(child);
}

#[tokio::test]
async fn test_watch_basic() {
  let t = TempDir::new();

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("test")
    .arg("--watch")
    .arg("--unstable")
    .arg("--no-check")
    .arg(t.path())
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);

  assert_eq!(next_line(&mut stdout_lines).await.unwrap(), "");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "0 passed | 0 failed"
  );
  wait_contains("Test finished", &mut stderr_lines).await;

  let foo_file = t.path().join("foo.js");
  let bar_file = t.path().join("bar.js");
  let foo_test = t.path().join("foo_test.js");
  let bar_test = t.path().join("bar_test.js");
  foo_file.write("export default function foo() { 1 + 1 }");
  bar_file.write("export default function bar() { 2 + 2 }");
  foo_test.write("import foo from './foo.js'; Deno.test('foo', foo);");
  bar_test.write("import bar from './bar.js'; Deno.test('bar', bar);");

  assert_eq!(next_line(&mut stdout_lines).await.unwrap(), "");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "running 1 test"
  );
  assert_contains!(next_line(&mut stdout_lines).await.unwrap(), "foo", "bar");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "running 1 test"
  );
  assert_contains!(next_line(&mut stdout_lines).await.unwrap(), "foo", "bar");
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  wait_contains("Test finished", &mut stderr_lines).await;

  // Change content of the file
  foo_test.write("import foo from './foo.js'; Deno.test('foobar', foo);");

  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Restarting");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "running 1 test"
  );
  assert_contains!(next_line(&mut stdout_lines).await.unwrap(), "foobar");
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  wait_contains("Test finished", &mut stderr_lines).await;

  // Add test
  let another_test = t.path().join("new_test.js");
  another_test.write("Deno.test('another one', () => 3 + 3)");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Restarting");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "running 1 test"
  );
  assert_contains!(next_line(&mut stdout_lines).await.unwrap(), "another one");
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  wait_contains("Test finished", &mut stderr_lines).await;

  // Confirm that restarting occurs when a new file is updated
  another_test.write("Deno.test('another one', () => 3 + 3); Deno.test('another another one', () => 4 + 4)");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Restarting");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "running 2 tests"
  );
  assert_contains!(next_line(&mut stdout_lines).await.unwrap(), "another one");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "another another one"
  );
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  wait_contains("Test finished", &mut stderr_lines).await;

  // Confirm that the watcher keeps on working even if the file is updated and has invalid syntax
  another_test.write("syntax error ^^");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Restarting");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "error:");
  assert_eq!(next_line(&mut stderr_lines).await.unwrap(), "");
  assert_eq!(
    next_line(&mut stderr_lines).await.unwrap(),
    "  syntax error ^^"
  );
  assert_eq!(
    next_line(&mut stderr_lines).await.unwrap(),
    "         ~~~~~"
  );
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Test failed");

  // Then restore the file
  another_test.write("Deno.test('another one', () => 3 + 3)");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Restarting");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "running 1 test"
  );
  assert_contains!(next_line(&mut stdout_lines).await.unwrap(), "another one");
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  wait_contains("Test finished", &mut stderr_lines).await;

  // Confirm that the watcher keeps on working even if the file is updated and the test fails
  // This also confirms that it restarts when dependencies change
  foo_file
    .write("export default function foo() { throw new Error('Whoops!'); }");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Restarting");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "running 1 test"
  );
  assert_contains!(next_line(&mut stdout_lines).await.unwrap(), "FAILED");
  wait_contains("FAILED", &mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  wait_contains("Test failed", &mut stderr_lines).await;

  // Then restore the file
  foo_file.write("export default function foo() { 1 + 1 }");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Restarting");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "running 1 test"
  );
  assert_contains!(next_line(&mut stdout_lines).await.unwrap(), "foo");
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  next_line(&mut stdout_lines).await;
  wait_contains("Test finished", &mut stderr_lines).await;

  // Test that circular dependencies work fine
  foo_file.write("import './bar.js'; export default function foo() { 1 + 1 }");
  bar_file.write("import './foo.js'; export default function bar() { 2 + 2 }");
  check_alive_then_kill(child);
}

#[flaky_test]
#[tokio::main]
async fn test_watch_doc() {
  let t = TempDir::new();

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("test")
    .arg("--watch")
    .arg("--doc")
    .arg("--unstable")
    .arg(t.path())
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);

  assert_eq!(next_line(&mut stdout_lines).await.unwrap(), "");
  assert_contains!(
    next_line(&mut stdout_lines).await.unwrap(),
    "0 passed | 0 failed"
  );
  wait_contains("Test finished", &mut stderr_lines).await;

  let foo_file = t.path().join("foo.ts");
  foo_file.write(
    r#"
    export default function foo() {}
  "#,
  );

  foo_file.write(
    r#"
    /**
     * ```ts
     * import foo from "./foo.ts";
     * ```
     */
    export default function foo() {}
  "#,
  );

  // We only need to scan for a Check file://.../foo.ts$3-6 line that
  // corresponds to the documentation block being type-checked.
  assert_contains!(skip_restarting_line(&mut stderr_lines).await, "foo.ts$3-6");
  check_alive_then_kill(child);
}

#[tokio::test]
async fn test_watch_module_graph_error_referrer() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  file_to_watch.write("import './nonexistent.js';");
  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg("--unstable")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (_, mut stderr_lines) = child_lines(&mut child);
  let line1 = next_line(&mut stderr_lines).await.unwrap();
  assert_contains!(&line1, "Process started");
  let line2 = next_line(&mut stderr_lines).await.unwrap();
  assert_contains!(&line2, "error: Module not found");
  assert_contains!(&line2, "nonexistent.js");
  let line3 = next_line(&mut stderr_lines).await.unwrap();
  assert_contains!(&line3, "    at ");
  assert_contains!(&line3, "file_to_watch.js");
  wait_contains("Process failed", &mut stderr_lines).await;
  check_alive_then_kill(child);
}

// Regression test for https://github.com/denoland/deno/issues/15428.
#[tokio::test]
async fn test_watch_unload_handler_error_on_drop() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  file_to_watch.write(
    r#"
    addEventListener("unload", () => {
      throw new Error("foo");
    });
    setTimeout(() => {
      throw new Error("bar");
    });
    "#,
  );
  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (_, mut stderr_lines) = child_lines(&mut child);
  wait_contains("Process started", &mut stderr_lines).await;
  wait_contains("Uncaught Error: bar", &mut stderr_lines).await;
  wait_contains("Process failed", &mut stderr_lines).await;
  check_alive_then_kill(child);
}

#[tokio::test]
async fn run_watch_blob_urls_reset() {
  let _g = util::http_server();
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  let file_content = r#"
    const prevUrl = localStorage.getItem("url");
    if (prevUrl == null) {
      console.log("first run, storing blob url");
      const url = URL.createObjectURL(
        new Blob(["export {}"], { type: "application/javascript" }),
      );
      await import(url); // this shouldn't insert into the fs module cache
      localStorage.setItem("url", url);
    } else {
      await import(prevUrl)
        .then(() => console.log("importing old blob url incorrectly works"))
        .catch(() => console.log("importing old blob url correctly failed"));
    }
    "#;
  file_to_watch.write(file_content);
  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);
  wait_contains("first run, storing blob url", &mut stdout_lines).await;
  wait_contains("finished", &mut stderr_lines).await;
  file_to_watch.write(file_content);
  wait_contains("importing old blob url correctly failed", &mut stdout_lines)
    .await;
  wait_contains("finished", &mut stderr_lines).await;
  check_alive_then_kill(child);
}

#[cfg(unix)]
#[tokio::test]
async fn test_watch_sigint() {
  use nix::sys::signal;
  use nix::sys::signal::Signal;
  use nix::unistd::Pid;

  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  file_to_watch.write(r#"Deno.test("foo", () => {});"#);
  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("test")
    .arg("--watch")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);
  wait_contains("Test started", &mut stderr_lines).await;
  wait_contains("ok | 1 passed | 0 failed", &mut stdout_lines).await;
  wait_contains("Test finished", &mut stderr_lines).await;
  signal::kill(Pid::from_raw(child.id() as i32), Signal::SIGINT).unwrap();
  let exit_status = child.wait().unwrap();
  assert_eq!(exit_status.code(), Some(130));
}

#[tokio::test]
async fn bench_watch_basic() {
  let t = TempDir::new();

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("bench")
    .arg("--watch")
    .arg("--unstable")
    .arg("--no-check")
    .arg(t.path())
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);

  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "Bench started"
  );
  assert_contains!(
    next_line(&mut stderr_lines).await.unwrap(),
    "Bench finished"
  );

  let foo_file = t.path().join("foo.js");
  let bar_file = t.path().join("bar.js");
  let foo_bench = t.path().join("foo_bench.js");
  let bar_bench = t.path().join("bar_bench.js");
  foo_file.write("export default function foo() { 1 + 1 }");
  bar_file.write("export default function bar() { 2 + 2 }");
  foo_bench.write("import foo from './foo.js'; Deno.bench('foo bench', foo);");
  bar_bench.write("import bar from './bar.js'; Deno.bench('bar bench', bar);");

  wait_contains("bar_bench.js", &mut stdout_lines).await;
  wait_contains("bar bench", &mut stdout_lines).await;
  wait_contains("foo_bench.js", &mut stdout_lines).await;
  wait_contains("foo bench", &mut stdout_lines).await;
  wait_contains("Bench finished", &mut stderr_lines).await;

  // Change content of the file
  foo_bench.write("import foo from './foo.js'; Deno.bench('foo asdf', foo);");

  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Restarting");
  loop {
    let line = next_line(&mut stdout_lines).await.unwrap();
    assert_not_contains!(line, "bar");
    if line.contains("foo asdf") {
      break; // last line
    }
  }
  wait_contains("Bench finished", &mut stderr_lines).await;

  // Add bench
  let another_test = t.path().join("new_bench.js");
  another_test.write("Deno.bench('another one', () => 3 + 3)");
  loop {
    let line = next_line(&mut stdout_lines).await.unwrap();
    assert_not_contains!(line, "bar");
    assert_not_contains!(line, "foo");
    if line.contains("another one") {
      break; // last line
    }
  }
  wait_contains("Bench finished", &mut stderr_lines).await;

  // Confirm that restarting occurs when a new file is updated
  another_test.write("Deno.bench('another one', () => 3 + 3); Deno.bench('another another one', () => 4 + 4)");
  loop {
    let line = next_line(&mut stdout_lines).await.unwrap();
    assert_not_contains!(line, "bar");
    assert_not_contains!(line, "foo");
    if line.contains("another another one") {
      break; // last line
    }
  }
  wait_contains("Bench finished", &mut stderr_lines).await;

  // Confirm that the watcher keeps on working even if the file is updated and has invalid syntax
  another_test.write("syntax error ^^");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Restarting");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "error:");
  assert_eq!(next_line(&mut stderr_lines).await.unwrap(), "");
  assert_eq!(
    next_line(&mut stderr_lines).await.unwrap(),
    "  syntax error ^^"
  );
  assert_eq!(
    next_line(&mut stderr_lines).await.unwrap(),
    "         ~~~~~"
  );
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Bench failed");

  // Then restore the file
  another_test.write("Deno.bench('another one', () => 3 + 3)");
  assert_contains!(next_line(&mut stderr_lines).await.unwrap(), "Restarting");
  loop {
    let line = next_line(&mut stdout_lines).await.unwrap();
    assert_not_contains!(line, "bar");
    assert_not_contains!(line, "foo");
    if line.contains("another one") {
      break; // last line
    }
  }
  wait_contains("Bench finished", &mut stderr_lines).await;

  // Test that circular dependencies work fine
  foo_file.write("import './bar.js'; export default function foo() { 1 + 1 }");
  bar_file.write("import './foo.js'; export default function bar() { 2 + 2 }");
  check_alive_then_kill(child);
}

// Regression test for https://github.com/denoland/deno/issues/15465.
#[tokio::test]
async fn run_watch_reload_once() {
  let _g = util::http_server();
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  let file_content = r#"
      import { time } from "http://localhost:4545/dynamic_module.ts";
      console.log(time);
    "#;
  file_to_watch.write(file_content);

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg("--reload")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);

  wait_contains("finished", &mut stderr_lines).await;
  let first_output = next_line(&mut stdout_lines).await.unwrap();

  file_to_watch.write(file_content);
  // The remote dynamic module should not have been reloaded again.

  wait_contains("finished", &mut stderr_lines).await;
  let second_output = next_line(&mut stdout_lines).await.unwrap();
  assert_eq!(second_output, first_output);

  check_alive_then_kill(child);
}

/// Regression test for https://github.com/denoland/deno/issues/18960. Ensures that Deno.serve
/// operates properly after a watch restart.
#[tokio::test]
async fn test_watch_serve() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  let file_content = r#"
      console.error("serving");
      await Deno.serve({port: 4600, handler: () => new Response("hello")});
    "#;
  file_to_watch.write(file_content);

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg("--unstable")
    .arg("--allow-net")
    .arg("-L")
    .arg("debug")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);

  wait_contains("Listening on", &mut stdout_lines).await;
  // Note that we start serving very quickly, so we specifically want to wait for this message
  wait_contains(r#"Watching paths: [""#, &mut stderr_lines).await;

  file_to_watch.write(file_content);

  wait_contains("serving", &mut stderr_lines).await;
  wait_contains("Listening on", &mut stdout_lines).await;

  check_alive_then_kill(child);
}

#[tokio::test]
async fn run_watch_dynamic_imports() {
  let t = TempDir::new();
  let file_to_watch = t.path().join("file_to_watch.js");
  file_to_watch.write(
    r#"
    console.log("Hopefully dynamic import will be watched...");
    await import("./imported.js");
    "#,
  );
  let file_to_watch2 = t.path().join("imported.js");
  file_to_watch2.write(
    r#"
    import "./imported2.js";
    console.log("I'm dynamically imported and I cause restarts!");
    "#,
  );
  let file_to_watch3 = t.path().join("imported2.js");
  file_to_watch3.write(
    r#"
    console.log("I'm statically imported from the dynamic import");
    "#,
  );

  let mut child = util::deno_cmd()
    .current_dir(util::testdata_path())
    .arg("run")
    .arg("--watch")
    .arg("--unstable")
    .arg("--allow-read")
    .arg("-L")
    .arg("debug")
    .arg(&file_to_watch)
    .env("NO_COLOR", "1")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .unwrap();
  let (mut stdout_lines, mut stderr_lines) = child_lines(&mut child);
  wait_contains("Process started", &mut stderr_lines).await;
  wait_contains("No package.json file found", &mut stderr_lines).await;

  wait_contains(
    "Hopefully dynamic import will be watched...",
    &mut stdout_lines,
  )
  .await;
  wait_contains(
    "I'm statically imported from the dynamic import",
    &mut stdout_lines,
  )
  .await;
  wait_contains(
    "I'm dynamically imported and I cause restarts!",
    &mut stdout_lines,
  )
  .await;

  wait_for_watcher("imported2.js", &mut stderr_lines).await;
  wait_contains("finished", &mut stderr_lines).await;

  file_to_watch3.write(
    r#"
    console.log("I'm statically imported from the dynamic import and I've changed");
    "#,
  );

  wait_contains("Restarting", &mut stderr_lines).await;
  wait_contains(
    "Hopefully dynamic import will be watched...",
    &mut stdout_lines,
  )
  .await;
  wait_contains(
    "I'm statically imported from the dynamic import and I've changed",
    &mut stdout_lines,
  )
  .await;
  wait_contains(
    "I'm dynamically imported and I cause restarts!",
    &mut stdout_lines,
  )
  .await;

  check_alive_then_kill(child);
}

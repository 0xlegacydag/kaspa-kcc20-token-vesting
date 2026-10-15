// Runs every check (`cargo test --frozen --no-fail-fast`) and passes only when the result is
// the one this repository expects: every test file ran, every check passed except `g2` and
// `g3` of tests/genesis.rs, and those two failed with their own message. They are forbidden
// moves that nothing on chain refuses (README.md, "What a launch must check"), so a green
// `cargo test` would mean that something changed. Prints the output of cargo as it comes,
// then one line with the counts; exits with 1 on any other result.
//
// Usage, in the root of the repository, with KCC20_REFERENCE_DIR naming a clone of the
// reference:
//   node tools/run-checks.mjs
// With VESTING_DUMP_DIR set, the checks also write the transactions they judged to
// <that folder>/cases.jsonl, the input of the replay in engine-2.1.0/; the folder must not
// hold one already, since the checks add to the file.

import { spawn } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { join, resolve } from "node:path";

// The two checks that fail on purpose, each with the start of its message.
const EXPECTED_FAILURES = {
  g2_the_key_coin_with_the_vesting_id_moves_the_whole_vault_at_once:
    "HOLE: a second coin with the vesting id moved the whole vault",
  g3_two_vesting_coins_release_the_schedule_twice:
    "HOLE: a second vesting coin of the same id released the schedule again",
};
// Every other check passes.
const EXPECTED_PASSED = 163;
// The builder of the testnet-10 run and its verifier: programs, not checks; Cargo.toml keeps
// them out of `cargo test` (testnet/README.md).
const PROGRAMS = new Set(["testnet_chain", "testnet_verify"]);

const here = resolve(".");
// Exit codes are set, not forced: a forced exit may cut what is still on its way to a pipe.
const stop = (message) => {
  console.log(`run-checks: ${message}`);
  process.exitCode = 1;
};

const reference = process.env.KCC20_REFERENCE_DIR;
const dump = process.env.VESTING_DUMP_DIR;
if (!reference || !existsSync(join(reference, "fixtures"))) {
  stop("KCC20_REFERENCE_DIR must name a clone of the KCC20 reference at c8a0871 (README.md)");
} else if (dump && existsSync(join(resolve(here, dump), "cases.jsonl"))) {
  stop(`${join(dump, "cases.jsonl")} exists already; remove it or name an empty folder`);
} else {
  run();
}

function run() {
  const testFiles = readdirSync(join(here, "tests"))
    .filter((name) => name.endsWith(".rs"))
    .map((name) => name.slice(0, -3))
    .filter((name) => !PROGRAMS.has(name));

  const cargo = spawn("cargo", ["test", "--frozen", "--no-fail-fast"], {
    cwd: here,
    stdio: ["ignore", "pipe", "pipe"],
  });
  // Each stream is kept apart, so that no line of one is cut by a chunk of the other.
  const out = [];
  const err = [];
  cargo.stdout.on("data", (chunk) => {
    process.stdout.write(chunk);
    out.push(chunk);
  });
  cargo.stderr.on("data", (chunk) => {
    process.stderr.write(chunk);
    err.push(chunk);
  });
  cargo.on("error", (error) => stop(`cargo did not start: ${error.message}`));
  cargo.on("close", (code) => {
    const lines = `${Buffer.concat(out)}\n${Buffer.concat(err)}`.split(/\r?\n/);
    const problems = [];

    // Every test file ran.
    const ran = new Set(
      lines
        .map((line) => /^\s*Running tests[\\/]([\w-]+)\.rs\b/.exec(line))
        .filter(Boolean)
        .map((match) => match[1]),
    );
    for (const name of testFiles) {
      if (!ran.has(name)) problems.push(`tests/${name}.rs did not run`);
    }

    // The counts of every test binary.
    let passed = 0;
    let failed = 0;
    let results = 0;
    for (const line of lines) {
      const match = /^test result: \w+\. (\d+) passed; (\d+) failed;/.exec(line);
      if (match) {
        results += 1;
        passed += Number(match[1]);
        failed += Number(match[2]);
      }
    }
    if (results === 0) problems.push("no test binary reported a result (a build error?)");

    // The failed checks, by name, and the message of each.
    const failedNames = lines
      .map((line) => /^test (\S+) \.\.\. FAILED$/.exec(line.trim()))
      .filter(Boolean)
      .map((match) => match[1]);
    for (const name of failedNames) {
      if (!(name in EXPECTED_FAILURES)) problems.push(`${name} failed`);
    }
    for (const [name, message] of Object.entries(EXPECTED_FAILURES)) {
      if (!failedNames.includes(name)) {
        problems.push(`${name} did not fail: the launch hole it shows is gone, or the check changed`);
        continue;
      }
      const start = lines.findIndex((line) => line.trim() === `---- ${name} stdout ----`);
      const end = lines.findIndex((line, index) => index > start && /^(---- |failures:$)/.test(line.trim()));
      const section = start < 0 ? [] : lines.slice(start, end < 0 ? undefined : end);
      if (!section.some((line) => line.includes(message))) {
        problems.push(`${name} failed without its message "${message}"`);
      }
    }
    if (failed !== Object.keys(EXPECTED_FAILURES).length) {
      problems.push(`${failed} checks failed, ${Object.keys(EXPECTED_FAILURES).length} expected`);
    }
    if (passed !== EXPECTED_PASSED) problems.push(`${passed} checks passed, ${EXPECTED_PASSED} expected`);
    if (code === 0) problems.push("cargo test passed, but g2 and g3 must fail");

    console.log("");
    if (problems.length) {
      console.log(`run-checks: NOT AS EXPECTED (cargo test exit ${code}):`);
      for (const problem of problems) console.log(`  ${problem}`);
      process.exitCode = 1;
      return;
    }
    console.log(
      `run-checks: ${passed + failed} checks in ${testFiles.length} test files; ${passed} passed; ` +
        `g2 and g3 failed with their own message, as they must (the launch hole).`,
    );
  });
}

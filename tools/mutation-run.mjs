// Checking the checks: takes one rule out of the contract at a time and runs every check
// against that weaker contract. A rule whose removal lets a forbidden move through is
// covered by that move; a rule whose removal changes nothing is either backed by another
// rule or not covered by any case.
//
// Usage, in the root of the repository, with KCC20_REFERENCE_DIR set, after one ordinary
// `cargo test`:
//   node tools/mutation-run.mjs              all rules
//   node tools/mutation-run.mjs 3 7          only the rules with these numbers
//   node tools/mutation-run.mjs --summarize  no runs: count again from the results on disk
//
// A weakened contract is CAUGHT when at least one check gives another result than it
// demands (a forbidden move accepted, or refused at another place than the one named), and
// SURVIVES when every check still passes as written. A survivor is a place where the
// checks are weaker than they look, unless no transaction can tell the two contracts apart.
//
// A run that fails is an ERROR, counted neither as caught nor as survived: in a mutation run
// (`VESTING_MUTANT` set) the checks write their records and assert nothing about the
// verdicts, so `cargo test` fails only when the run itself does (a build error, a missing
// setting such as `KCC20_REFERENCE_DIR`, a check that stopped before writing its record). A
// run that wrote no records is an error as well. The program exits with 1 when any run
// failed. `VESTING_DUMP_DIR` is not passed on: the weaker contracts' transactions do not
// belong in the dump of the replay.
//
// A rule is a single-line `require(...)` (replaced by `require(true);`) or one of the three
// output-state checks (`validateOutputState...(...)`, removed). Nothing in the repository is
// changed: the weaker sources and the results go to <target>/mutation.

import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync, existsSync, rmSync } from "node:fs";
import { join, resolve } from "node:path";

const here = resolve(".");
const target = process.env.CARGO_TARGET_DIR ? resolve(process.env.CARGO_TARGET_DIR) : join(here, "target");
const out = join(target, "mutation");
mkdirSync(out, { recursive: true });

const source = readFileSync(join(here, "contracts", "TokenVesting.sil"), "utf8");
const lines = source.split("\n");

// The rules, by line.
const rules = [];
for (let i = 0; i < lines.length; i++) {
  const text = lines[i].trim();
  if (/^require\(.*\);$/.test(text)) {
    rules.push({ start: i, end: i, kind: "require", text });
  } else if (/^validateOutputState(WithInputTemplate)?\($/.test(text)) {
    let end = i;
    while (!lines[end].trim().startsWith(");")) end++;
    rules.push({ start: i, end, kind: "validate", text: lines.slice(i, end + 1).map((l) => l.trim()).join(" ") });
  }
}

const began = new Date();
const summarizeOnly = process.argv.includes("--summarize");
const only = process.argv.slice(2).filter((a) => a !== "--summarize").map(Number);
const summary = [];
const { VESTING_DUMP_DIR: _dump, ...childEnv } = process.env;
rules.forEach((rule, index) => {
  const id = `R${String(index + 1).padStart(2, "0")}`;
  if (only.length && !only.includes(index + 1)) return;
  const mutated = [...lines];
  const indent = lines[rule.start].match(/^\s*/)[0];
  if (rule.kind === "require") {
    mutated[rule.start] = `${indent}require(true);`;
  } else {
    for (let i = rule.start; i <= rule.end; i++) mutated[i] = "";
    mutated[rule.start] = `${indent}require(true);`;
  }
  const file = join(out, `${id}.sil`);
  const results = join(out, id);
  let status = "ran";
  let error = null;
  if (summarizeOnly) {
    status = "counted from the results on disk";
  } else try {
    writeFileSync(file, mutated.join("\n"));
    if (existsSync(results)) rmSync(results, { recursive: true });
    mkdirSync(results, { recursive: true });
    execFileSync("cargo", ["test", "--frozen", "--test", "forbidden", "--test", "honest", "-q"], {
      cwd: here,
      env: { ...childEnv, VESTING_SOURCE_FILE: file, VESTING_MUTANT: id, CHECK_RESULTS_DIR: results },
      stdio: ["ignore", "pipe", "pipe"],
      maxBuffer: 1 << 28,
    });
  } catch (failure) {
    // The first panic and its message, else cargo's own error lines; absolute paths cut to
    // their file names.
    const text = `${failure.stdout || ""}\n${failure.stderr || ""}`.split(/\r?\n/).map((l) => l.trim());
    const panic = text.findIndex((l) => /panicked at/.test(l));
    const reason =
      panic >= 0
        ? `${text[panic]} ${text[panic + 1] || ""}`
        : text.filter((l) => /^error/.test(l)).slice(0, 2).join(" | ") || String(failure.message).split("\n")[0];
    error = `cargo test failed (exit ${failure.status ?? "none"}): ${reason.replace(/(?<=^|[\s`'"(])(?:[A-Za-z]:)?[\\/][^\s`'"]*[\\/]/g, "")}`;
    status = error;
  }
  const read = (name) =>
    existsSync(join(results, `${name}.jsonl`))
      ? readFileSync(join(results, `${name}.jsonl`), "utf8").trim().split("\n").filter(Boolean).map((l) => JSON.parse(l))
      : [];
  const forbidden = read("forbidden");
  const honest = read("honest");
  if (!error && (forbidden.length === 0 || honest.length === 0)) {
    error = "no results were written";
    status = error;
  }
  const accepted = forbidden.filter((r) => r.submit_accepted || r.block_accepted).map((r) => r.case);
  // The cases written for this very rule: what stops them once it is gone.
  const expectedHere = forbidden
    .filter((r) => {
      // Only the cases that name a statement of the vesting contract; a token-side case
      // names a statement of the token program, which may share words with ours.
      const expected = /vesting input stops at `(.*)`$/.exec(r.expectation || "");
      return expected && rule.text.includes(expected[1]);
    })
    .map((r) => ({
      case: r.case,
      now: r.submit_accepted || r.block_accepted ? "ACCEPTED" : `refused at ${r.inputs?.[0]?.place}`,
    }));
  const failingChecks = [...forbidden, ...honest].filter((r) => r.result !== "as expected").map((r) => r.case);
  const honestRefused = honest.filter((r) => !(r.submit_accepted && r.block_accepted) && !String(r.expectation).includes("mempool refuses")).map((r) => r.case);
  const entry = {
    id,
    line: rule.start + 1,
    rule: rule.text,
    status,
    forbidden_cases: forbidden.length,
    now_accepted: accepted,
    cases_written_for_this_rule: expectedHere,
    honest_cases: honest.length,
    honest_refused: honestRefused,
    failing_checks: failingChecks,
    verdict: error ? "ERROR" : failingChecks.length ? "caught" : "SURVIVED",
  };
  summary.push(entry);
  console.log(`${id} line ${entry.line}: ${entry.verdict}, ${failingChecks.length} checks fail; ${accepted.length} forbidden moves now accepted; honest refused ${honestRefused.length}; ${status} | ${rule.text.slice(0, 90)}`);
  for (const name of accepted) console.log(`    accepted: ${name}`);
  for (const c of expectedHere) if (c.now !== "ACCEPTED") console.log(`    written for it, now ${c.now.slice(0, 160)}: ${c.case}`);
});
writeFileSync(join(out, "summary.json"), JSON.stringify(summary, null, 2));
const ran = summary.filter((e) => e.verdict !== "ERROR");
const caught = ran.filter((e) => e.verdict === "caught");
const survivors = ran.filter((e) => e.verdict === "SURVIVED");
console.log(`\n${summary.length} rules weakened; the checks caught ${caught.length}; ${survivors.length} survived:`);
for (const e of survivors) console.log(`  ${e.id} line ${e.line}: ${e.rule.slice(0, 100)}`);
const uncovered = ran.filter((e) => e.now_accepted.length === 0);
console.log(`${ran.length - uncovered.length} let at least one forbidden move through; ${uncovered.length} let none:`);
for (const e of uncovered) console.log(`  ${e.id} line ${e.line}: ${e.rule.slice(0, 100)}`);
const errors = summary.filter((e) => e.verdict === "ERROR");
console.log(`${errors.length} runs failed, counted neither as caught nor as survived${errors.length ? ":" : "."}`);
for (const e of errors) console.log(`  ${e.id} line ${e.line}: ${e.status.slice(0, 300)}`);
const ended = new Date();
const time = (date) => date.toISOString().slice(11, 19);
console.log(`started ${time(began)} UTC, ended ${time(ended)} UTC: ${Math.round((ended - began) / 60000)} minutes`);
if (errors.length) process.exitCode = 1;

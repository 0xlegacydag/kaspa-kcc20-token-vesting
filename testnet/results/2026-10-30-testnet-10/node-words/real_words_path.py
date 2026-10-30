"""The node's real answer of 2026-10-30, word for word, put to the sender's `send` through its fake node.
Run from the sender's folder (testnet/) with its environment. Nothing goes to a network."""
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, "tests")
sys.path.insert(0, ".")
import test_vesting_send as t  # noqa: E402
vs = t.vs

RECORD = Path(__file__).parent / "double-spend.jsonl"
lines = [json.loads(line) for line in RECORD.read_text(encoding="utf-8").splitlines()]
ask = next(line for line in lines if "first" in line)
answer = next(line for line in lines if "second_answer" in line)
REAL = answer["second_answer"]["words"]
COIN = ask["coin"].split(":")[0]
print("the node's words:", REAL)
print("spender named   :", vs.SPENDER_NAMED.search(REAL).group(1), "(the first transaction:", ask["first"] + ")")
assert vs.SPENDER_NAMED.search(REAL).group(1) == ask["first"]
assert f"{vs.REFUSAL_MARK} {ask['second']}" in REAL
STRANGER = "ab" * 32


def words_for(probe_id: str, coin: str, spender: str) -> str:
    """The real line with this run's ids in it: nothing else changes."""
    return REAL.replace(ask["second"], probe_id).replace(COIN, coin).replace(ask["first"], spender)


class RealWords(t.Send):
    def test_a_stranger_named(self):
        node = t.FakeNode()
        node.how[t.IDS[t.B1]] = "raw:" + words_for(t.IDS[t.B1], t.IDS[t.A2], STRANGER)
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_INCONSISTENT, printed)
        self.assertNotIn("FORBIDDEN MOVE", printed)
        self.assertIn("the node did not take it and names a transaction this file does not know", printed)
        taken = [event for event in self.events() if event["what"] == vs.PROBE_COIN_TAKEN]
        self.assertEqual((taken[0]["probe"], taken[0]["spender"]), (t.B1, STRANGER))
        print("stranger: exit", code, "- line:", [line for line in printed.splitlines() if "did not take it" in line][0][:200])
        del node.how[t.IDS[t.B1]]
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_OK, printed)
        print("second run with that record: exit", code, "(the probe was handed over again, no lasting block)")

    def test_this_probe_named(self):
        node = t.FakeNode()
        node.how[t.IDS[t.B1]] = "raw:" + words_for(t.IDS[t.B1], t.IDS[t.A2], t.IDS[t.B1])
        code, printed = self.send(node)
        self.assertEqual(code, vs.EXIT_FORBIDDEN_ACCEPTED, printed)
        print("this probe named: exit", code)


unittest.main(argv=["x", "RealWords.test_a_stranger_named", "RealWords.test_this_probe_named"], exit=True, verbosity=1)

<!--
Thanks for contributing. The build and test commands, the lint policy and the two rules below are
explained in https://github.com/datadog-labs/sqleq/blob/main/CONTRIBUTING.md.
-->

## What type of PR is this?

- [ ] Soundness fix (a prover could claim equivalence for a pair that is not equivalent)
- [ ] Coverage (the frontend now lowers something it used to refuse)
- [ ] Bug fix
- [ ] Feature / refactor
- [ ] Documentation

## Description

## How it was tested

## Checklist

- [ ] A construct the frontend cannot lower faithfully is refused, not approximated.
- [ ] If this grows the provable set, the same pairs were run through `sqleq-check --portfolio`
      (the provers and `sqleq-fuzz` at once; a proof with a counterexample is an `alarm`), and none
      of the newly proven pairs has a counterexample.
- [ ] If this fixes a defect that can be stated as a pair, the pair is pinned under `tests/pairs/`
      and was seen failing on a build from before the fix.
- [ ] Every pin under `tests/pairs/` that moved is explained in the description.
- [ ] If dependencies changed, `LICENSE-3rdparty.csv` was regenerated with
      `sh tools/update_license_3rdparty.sh`.

## AI code assistants

_If an AI code assistant helped with this contribution, name it here._

Assisted-by: NAME_OF_CODE_ASSISTANT

#!/bin/bash

echo WARNING: this assumes that HEAD is aosp/upstream-main

git diff HEAD..aosp/upstream-main --stat -- $(find . -name "*.rs" -o -name "*.toml")

function crosvm_take_theirs() {
    git checkout aosp/upstream-main -- $(find . -name "*.rs" -o -name "*.toml" | grep -v "/out/")
}

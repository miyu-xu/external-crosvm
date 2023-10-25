#!/bin/bash

# Finds all the rust_test modules and outputs them in an easy to copy paste
# format for the TEST_MAPPING file.

rg -g Android.bp -A1 rust_test | awk '/name/ {
    name = $3
    sub(/,/, "", name)
    print "    {\"name\": " name "},"
}' | sort

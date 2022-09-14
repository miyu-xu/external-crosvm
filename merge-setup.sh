#!/bin/bash

rustup update
./external/crosvm/tools/install-deps
source $ANDROID_BUILD_TOP/build/envsetup.sh
m blueprint_tools
repo sync -c -j96
m
cd $ANDROID_BUILD_TOP/external/crosvm
git fetch --all --prune
repo start merge
git merge --log aosp/upstream-main

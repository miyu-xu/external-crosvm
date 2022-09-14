#!/bin/bash

set -ex

read -p "This script will sync your crosvm project. Do you wish to proceed? [y/N]" -n 1 -r
if [[ ! $REPLY =~ ^[Yy]$ ]]
then
  exit 1;
fi

cd $ANDROID_BUILD_TOP/external/crosvm

rustup update
if [ -z $ANDROID_BUILD_TOP ]; then echo "forgot to lunch?" && exit 1; fi
repo sync . -c -j96
if ! [[ -z $(git branch --list merge) ]];
  then
    echo "branch merge already exists. Forgot to clean up?" && exit 1;
fi
source $ANDROID_BUILD_TOP/build/envsetup.sh
m blueprint_tools
m crosvm
git fetch --all --prune
repo start merge
git merge --log aosp/upstream-main
./external/crosvm/tools/install-deps

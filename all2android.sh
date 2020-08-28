#!/bin/bash

# Convenience script to run cargo2android.py with the appropriate arguments in the crosvm directory
# and all subdirectories with Cargo.toml files.

set -e

# Run in the main crosvm directory.
cargo2android.py --run --device --tests --dependencies --no-subdir
rm -r target.tmp cargo.out

for dir in */src
do
  base=`dirname $dir`
  echo "$base"
  cd "$base"
  # If the subdirectory has more subdirectories with crates, then pass --no-subdir and run it in
  # each of them too.
  if compgen -G "*/Cargo.toml" > /dev/null
  then
    cargo2android.py --run --device --tests --dependencies --global_defaults=crosvm_defaults --add_workspace --no-subdir
    rm -r cargo.out target.tmp

    for dir in */Cargo.toml
    do
      sub_base=`dirname $dir`
      echo "$base/$sub_base"
      cd "$sub_base"
      cargo2android.py --run --device --tests --dependencies --global_defaults=crosvm_defaults --add_workspace
      rm -r cargo.out target.tmp
      cd ..
    done
  else
    cargo2android.py --run --device --tests --dependencies --global_defaults=crosvm_defaults --add_workspace
    rm -r cargo.out target.tmp
  fi

  cd ..
done

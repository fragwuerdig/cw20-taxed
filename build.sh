#! /bin/bash

set -e

BLACKLIST=""

rm -Rf artifacts
mkdir artifacts
cargo build --release --target wasm32-unknown-unknown

mv target/wasm32-unknown-unknown/release/*.wasm artifacts/
rm -Rf target
for i in $BLACKLIST; do
    rm -f artifacts/$i.wasm
done

cd artifacts
mkdir -p opt
for i in *.wasm; do
    wasm-opt -Os --strip-debug $i -o opt/$i
done

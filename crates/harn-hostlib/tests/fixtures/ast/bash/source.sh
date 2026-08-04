#!/usr/bin/env bash

greet() {
    echo "hello $1"
}

function shout {
    echo "$1" | tr '[:lower:]' '[:upper:]'
}

main() {
    greet "world"
    shout "hi"
}

main "$@"

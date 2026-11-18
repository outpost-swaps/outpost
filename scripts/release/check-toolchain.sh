#!/bin/sh
# Release gate: print and validate the JDK, Gradle and JavaFX versions the build uses.
# Fails if any of them is not the supported release line.
set -eu
cd "$(dirname "$0")/../.."

REQUIRED_JDK=25
REQUIRED_GRADLE=9.8
REQUIRED_JAVAFX=25

fail=0
check() { # name actual wanted-prefix
    if [ "${2#"$3"}" = "$2" ]; then
        printf 'FAIL  %-8s %s (want %s*)\n' "$1" "$2" "$3"
        fail=1
    else
        printf 'ok    %-8s %s\n' "$1" "$2"
    fi
}

jdk=$(java -version 2>&1 | sed -n '1s/.*version "\([^"]*\)".*/\1/p')
java -version 2>&1 | sed -n '1,3p'
check "java" "$jdk" "$REQUIRED_JDK"

gradle=$(./gradlew --version 2>/dev/null | sed -n 's/^Gradle \(.*\)$/\1/p' | head -1)
check "gradle" "$gradle" "$REQUIRED_GRADLE"

javafx=$(sed -n 's/^ *version = "\([0-9][0-9.]*\)".*/\1/p' app/build.gradle | head -1)
check "javafx" "$javafx" "$REQUIRED_JAVAFX"

wrapper=$(sed -n 's/.*gradle-\(.*\)-bin.zip/\1/p' gradle/wrapper/gradle-wrapper.properties)
check "wrapper" "$wrapper" "$REQUIRED_GRADLE"

exit "$fail"

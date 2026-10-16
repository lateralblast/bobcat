#!/bin/sh
# Rebuild ilohost.jar (embedded in bobcat) from IloHost.java. Needs a JDK (javac, jar); Java 8 class files.
set -e
cd "$(dirname "$0")"
rm -rf classes && mkdir classes
javac --release 8 -Xlint:-options -d classes IloHost.java
jar cf ilohost.jar -C classes .
rm -rf classes
ls -l ilohost.jar

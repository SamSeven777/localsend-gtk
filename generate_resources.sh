#!/usr/bin/env sh
set -eu
# Cargo runs the same compiler automatically into OUT_DIR; this is optional.
glib-compile-resources gresources.xml --target=resources.gresource

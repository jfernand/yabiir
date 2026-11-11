#!/bin/sh
# build.sh: compiles the book and marks the PDF to open with its bookmarks (outline) panel showing.
# Typst writes the outline but no /PageMode, so viewers open with the sidebar closed.
# The flag is added as an incremental update; Typst's bytes are left as they are.
set -e
cd "$(dirname "$0")/.."
typst compile databases.typ "$@"
python3 -I - <<'PY'
from pypdf import PdfWriter
w = PdfWriter("databases.pdf", incremental=True)
w.page_mode = "/UseOutlines"
w.write("databases.pdf")
PY

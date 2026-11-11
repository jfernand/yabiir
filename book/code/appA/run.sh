#!/bin/sh
# Runs the appendix's SQL examples in an in-memory SQLite database.
# Usage (from the repository root): book/code/appA/run.sh
set -e
sqlite3 --version | cut -d' ' -f1-2
sqlite3 :memory: < book/code/appA/bags-and-nulls.sql

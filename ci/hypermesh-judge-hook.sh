#!/bin/sh
# Later hook for an internal hyperme.sh test key.
# GitHub Actions does not run this script.
# When that task API is robust, an internal runner can start real tasks from here
# and judge the results. This file does not call the API and does not read a secret.
echo "hyperme.sh task judge is not wired" >&2
exit 0

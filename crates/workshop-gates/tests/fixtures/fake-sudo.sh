#!/bin/sh
# A stand-in `sudo` for the askpass gate. Like the real one run without a terminal: with no
# SUDO_ASKPASS it fails at once ("a terminal is required…"); with one it runs the helper with
# sudo's prompt, reads the password from the helper's stdout, and runs the command when it is
# `hunter2`. A helper that exits non-zero ends the attempt ("no password was provided"); a wrong
# password is retried up to three times ("Sorry, try again."). Never prompts on a tty itself.
if [ "$1" = "-n" ]; then
  echo "sudo: a password is required" >&2
  exit 1
fi
if [ -z "$SUDO_ASKPASS" ]; then
  echo "sudo: a terminal is required to read the password; either use the -S option to read from standard input or configure an askpass helper" >&2
  echo "sudo: a password is required" >&2
  exit 1
fi
tries=0
while [ "$tries" -lt 3 ]; do
  pw=$("$SUDO_ASKPASS" "[sudo] password for tester: ") || {
    echo "sudo: no password was provided" >&2
    exit 1
  }
  if [ "$pw" = "hunter2" ]; then
    exec "$@"
  fi
  echo "Sorry, try again." >&2
  tries=$((tries + 1))
done
echo "sudo: 3 incorrect password attempts" >&2
exit 1

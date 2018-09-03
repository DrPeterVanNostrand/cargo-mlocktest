# cargo-mlocktest

A cargo subcommand to monitor the ammount of memory locked into RAM during
`cargo test`

**Note: `cargo-mlocktest` only runs on Linux.**

This subcommand is useful when debugging `SIGILL` segfaults that may arise
when testing rust programs that lock large ammounts of memory into RAM.

### Usage

You can use this `cargo` subcommand by running the following:

```
# Install the `mlocktest` subcommand:
$ cd cargo-mlocktest
$ cargo build
$ cp target/debug/cargo-mlocktest ~/.cargo/bin/

# Use the subcommand:
$ cd <different rust project>
$ cargo mlocktest

# When you no longer need the subcommand, run the following to uninstall:
$ rm ~/.cargo/bin/cargo-mlocktest
```

### Output

Running `cargo mlocktest` will run `cargo test`, then output the following
table:

```
Mlock Monitor for `cargo test`
===============================
Locked memory limit (soft, kb): <your systems soft locked memory limit>
Lock memory limit (hard, kb): <your systems hard locked memory limit>

Running `cargo test` ... done!

Process Name                            Max Locked Memory (kb)
============                            ======================
<test 1 binary name>        		<the max number of kbs locked during test 1>
<test 2 binary name>                    <the max number of kbs locked during test 2>
...
==============================================================
```

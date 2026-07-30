# xacli

This package distributes the native Rust [`xa`](https://github.com/jinfagang/xa)
command-line program through PyPI. It contains no Python runtime wrapper.

```bash
pipx install xacli
# or: python -m pip install xacli
xa --help
```

`pip` installs `xa` into the scripts directory of the selected Python
environment (for example, a virtual environment's `bin/`, or `Scripts\\` on
Windows). Use `pipx` for a globally available command without modifying the
system Python installation.

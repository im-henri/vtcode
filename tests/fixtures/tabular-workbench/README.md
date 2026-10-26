# Tabular Data Workbench host test fixture

This is the real release Wasm module, manifest, sample data, and licenses.
Its source is the standalone
[Tabular Data Workbench repository](https://github.com/im-henri/vtcode-ext-tabular-workbench/tree/0361e157827668642713b41b6896a1a8b21763d9).
The fixture includes the source and regression-test changes in
[`profile-top-frequency.patch`](profile-top-frequency.patch), which avoid sorting
every unique value when only the 24 most frequent values are displayed.
Apply that patch to the revision above before rebuilding the standalone package:

    git apply <vtcode-source>/tests/fixtures/tabular-workbench/profile-top-frequency.patch

    python tools/build.py --host-fixtures <vtcode-source>

Run the VTCode integration suite with:

    cargo test --bin vtcode extension_tabular_tests -- --test-threads=1

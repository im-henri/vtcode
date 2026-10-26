# Official ACP Python examples

`echo_agent.py` and `agent.py` are unmodified examples from the official
[ACP Python SDK 0.12.1 source release](https://pypi.org/project/agent-client-protocol/0.12.1/),
maintained in [agentclientprotocol/python-sdk](https://github.com/agentclientprotocol/python-sdk).
The download URL and verified SHA-256 digest are recorded in `REVISION`.
The upstream Apache-2.0 license is included in `LICENSE`.

The integration suite launches `echo_agent.py` directly as an external stdio
process. `../scenario_agent.py` extends upstream `ExampleAgent` for deterministic
client callbacks, cancellation, and error scenarios, using the official Python
SDK's protocol implementation. Neither peer shares vtcode's Rust transport.

SDK 0.12.1 ships the `session/delete` schema and method constant but omits its
agent-router registration. The scenario subclass registers that method through
the SDK's `route_request`, using its official request validator and dispatcher.
The upstream example files and installed SDK remain unmodified.

From the repository root, install the pinned dependencies with
`python -m pip install -r tests/fixtures/requirements.txt`. Set
`VTCODE_ACP_TEST_PYTHON` to that interpreter, then run
`cargo test --locked --test acp_integration -- --ignored --test-threads=1`.
Once installed, the tests run without network access.

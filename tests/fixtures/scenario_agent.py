"""Deterministic scenarios layered on the official ACP SDK ExampleAgent.

The unmodified upstream echo agent is tested separately. This subclass exercises
the client-facing half of the protocol through the independent Python SDK.
"""
import asyncio
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent / "acp-python-sdk"))
from agent import ExampleAgent
from acp import run_agent
from acp import schema as s
from acp.core import RequestError

# SDK 0.12.1 includes the official delete types and method name, but its
# agent router omits their registration. Add the route using the SDK's own
# validator/dispatcher; framing and all protocol handling remain SDK-owned.
import acp.agent.connection as sdk_connection
from acp.meta import AGENT_METHODS
_sdk_router = sdk_connection.build_agent_router


def scenario_router(agent, use_unstable_protocol=False):
    router = _sdk_router(agent, use_unstable_protocol=use_unstable_protocol)
    router.route_request(AGENT_METHODS["session_delete"], s.DeleteSessionRequest, agent, "delete_session")
    return router


sdk_connection.build_agent_router = scenario_router


class ScenarioAgent(ExampleAgent):
    def __init__(self):
        super().__init__()
        self.cwd = {}
        self.cancelled = {}
        self.authenticated = False
        self.mode_a = os.environ.get("ACP_TEST_MODE_A", "inspect-37")
        self.mode_b = os.environ.get("ACP_TEST_MODE_B", "build-91")
        self.current_mode = self.mode_a
        self.detail = "brief"
        self.model_a = os.environ.get("ACP_TEST_MODEL_A", "engine-47")
        self.model_b = os.environ.get("ACP_TEST_MODEL_B", "engine-91")
        self.current_model = self.model_a
        self.thought = "balanced-52"
        self.context = "ctx-128"
        self.references = True
        self.deleted = set()

    def edge_log(self, event, session_id):
        if path := os.environ.get("ACP_TEST_EDGE_LOG"):
            with open(path, "a", encoding="utf-8") as log:
                log.write(json.dumps({"event": event, "session": session_id}) + "\n")

    def modes(self):
        return s.SessionModeState(current_mode_id=self.current_mode, available_modes=[
            s.SessionMode(id=self.mode_a, name="Inspect", description="Explore the workspace"),
            s.SessionMode(id=self.mode_b, name="Build", description="Make and verify changes"),
        ])

    def options(self):
        options = [s.SessionConfigOptionSelect(type="select", id="detail-level", name="Response detail", current_value=self.detail,
                options=[s.SessionConfigSelectOption(value="brief", name="Brief"), s.SessionConfigSelectOption(value="full", name="Full")])]
        if os.environ.get("ACP_TEST_CHAT_SELECTORS"):
            context_values = [s.SessionConfigSelectOption(value="ctx-128", name="128K")]
            thought_values = [s.SessionConfigSelectOption(value="balanced-52", name="Balanced"), s.SessionConfigSelectOption(value="deep-93", name="High")]
            if self.current_model == self.model_b:
                context_values.append(s.SessionConfigSelectOption(value="ctx-256", name="256K"))
                thought_values.append(s.SessionConfigSelectOption(value="extended-901", name="Maximum"))
            options = [
                s.SessionConfigOptionSelect(type="select", id="behavior-choice", name="Behavior", category="mode", current_value=self.current_mode,
                    options=[s.SessionConfigSelectOption(value=self.mode_a, name="Ask"), s.SessionConfigSelectOption(value=self.mode_b, name="Plan")]),
                s.SessionConfigOptionSelect(type="select", id="engine-choice", name="Engine", category="model", current_value=self.current_model,
                    options=[s.SessionConfigSelectOption(value=self.model_a, name="Workspace Fast"), s.SessionConfigSelectOption(value=self.model_b, name="Workspace Large")]),
                s.SessionConfigOptionSelect(type="select", id="budget-control", name="Context", category="model_config", current_value=self.context, options=context_values),
                s.SessionConfigOptionSelect(type="select", id="effort-control", name="Thinking", category="thought_level", current_value=self.thought, options=thought_values),
                *options,
                s.SessionConfigOptionBoolean(type="boolean", id="include-references", name="References", category="_citations", current_value=self.references),
            ]
        return options

    async def initialize(self, protocol_version, client_capabilities=None, **kwargs):
        if os.environ.get("ACP_TEST_INIT_HANG"):
            await asyncio.sleep(120)
        if path := os.environ.get("ACP_TEST_PID_FILE"):
            Path(path).write_text(str(os.getpid()), encoding="utf-8")
        if path := os.environ.get("ACP_TEST_CHILD_PID_FILE"):
            child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(120)"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            Path(path).write_text(str(child.pid), encoding="utf-8")
        assert client_capabilities.fs.read_text_file
        assert client_capabilities.fs.write_text_file
        assert client_capabilities.terminal
        if os.environ.get("ACP_TEST_CHAT_SELECTORS"):
            assert client_capabilities.session.config_options.boolean is not None
            assert client_capabilities.model_dump(by_alias=True).get("_meta", {}).get("parameterizedModelPicker") is True
        return s.InitializeResponse(protocol_version=99 if os.environ.get("ACP_TEST_BAD_VERSION") else 1,
            agent_info=s.Implementation(name="sdk-scenarios", title="Python SDK Test Agent", version="1.0"),
            agent_capabilities=s.AgentCapabilities(load_session=not bool(os.environ.get("ACP_TEST_RESUME_ONLY")), prompt_capabilities=s.PromptCapabilities(embedded_context=bool(os.environ.get("ACP_TEST_EMBEDDED_CONTEXT"))), session_capabilities=s.SessionCapabilities(list=s.SessionListCapabilities(), delete=None if os.environ.get("ACP_TEST_NO_DELETE") else s.SessionDeleteCapabilities(), close=s.SessionCloseCapabilities(), resume=s.SessionResumeCapabilities())),
            auth_methods=[s.AuthMethodAgent(id="local-handshake", name="Local test sign-in")])

    async def authenticate(self, method_id, **kwargs):
        assert method_id == "local-handshake"
        self.authenticated = True
        return s.AuthenticateResponse()

    async def new_session(self, cwd, **kwargs):
        if os.environ.get("ACP_TEST_REQUIRE_AUTH") and not self.authenticated:
            raise RequestError(-32000, "Authenticate using the advertised local method")
        result = await super().new_session(cwd, **kwargs)
        self.edge_log("new-session", result.session_id)
        self.cwd[result.session_id] = cwd
        if os.environ.get("ACP_TEST_EARLY_UPDATES"):
            await self._send_agent_message(result.session_id, s.TextContentBlock(type="text", text="Welcome before session response"))
        return s.NewSessionResponse(session_id=result.session_id, modes=self.modes(), config_options=self.options())

    async def load_session(self, cwd, session_id, **kwargs):
        self.cwd[session_id] = cwd
        self._sessions.add(session_id)
        self.edge_log("load-start", session_id)
        if directory := os.environ.get("ACP_TEST_CONTROL_GATE_DIR"):
            gates = Path(directory)
            (gates / "load-ready").write_text("ready", encoding="utf-8")
            while not (gates / "finish-load").exists():
                await asyncio.sleep(0.01)
        if not os.environ.get("ACP_TEST_EMPTY_REPLAY"):
            await self._send_agent_message(session_id, s.TextContentBlock(type="text", text="Restored history\n"))
        self.edge_log("load-finished", session_id)
        return s.LoadSessionResponse(modes=self.modes(), config_options=self.options())

    async def list_sessions(self, cwd=None, cursor=None, **kwargs):
        session_id = "history-b" if cursor else "history-a"
        sessions = [] if session_id in self.deleted else [s.SessionInfo(session_id=session_id, cwd=cwd or str(Path.cwd()), title="Saved conversation")]
        if os.environ.get("ACP_TEST_LIVE_HISTORY") and not cursor:
            sessions.extend(s.SessionInfo(session_id=sid, cwd=path) for sid, path in self.cwd.items() if sid not in self.deleted)
        return s.ListSessionsResponse(sessions=sessions, next_cursor=None if cursor else "page-2")

    async def delete_session(self, session_id, **kwargs):
        if os.environ.get("ACP_TEST_NO_DELETE"):
            raise AssertionError("Client called an unadvertised method")
        if session_id == "delete-fails":
            raise RequestError(-32000, "Test deletion failed")
        self.deleted.add(session_id)
        self._sessions.discard(session_id)
        if log := os.environ.get("ACP_TEST_DELETE_LOG"):
            with open(log, "a", encoding="utf-8") as file:
                file.write(session_id + "\n")
        return s.DeleteSessionResponse()

    async def resume_session(self, cwd, session_id, **kwargs):
        self.cwd[session_id] = cwd
        self._sessions.add(session_id)
        return s.ResumeSessionResponse(modes=self.modes(), config_options=self.options())

    async def close_session(self, session_id, **kwargs):
        self._sessions.discard(session_id)
        return s.CloseSessionResponse()

    async def set_session_mode(self, session_id, mode_id, **kwargs):
        if mode_id not in [self.mode_a, self.mode_b]:
            raise RequestError(-32602, "Unknown dynamic mode")
        self.current_mode = mode_id
        await self._conn.session_update(session_id, s.CurrentModeUpdate(session_update="current_mode_update", current_mode_id=mode_id))
        return s.SetSessionModeResponse()

    async def set_config_option(self, session_id, config_id, value, **kwargs):
        if config_id == "engine-choice" and os.environ.get("ACP_TEST_CHAT_SELECTORS"):
            assert value in [self.model_a, self.model_b]
            self.current_model = value
            self.context = "ctx-256" if value == self.model_b else "ctx-128"
            if value == self.model_a and self.thought == "extended-901":
                self.thought = "balanced-52"
        elif config_id == "behavior-choice" and os.environ.get("ACP_TEST_CHAT_SELECTORS"):
            assert value in [self.mode_a, self.mode_b]
            self.current_mode = value
        elif config_id == "budget-control" and os.environ.get("ACP_TEST_CHAT_SELECTORS"):
            assert value == "ctx-128" or (value == "ctx-256" and self.current_model == self.model_b)
            self.context = value
        elif config_id == "effort-control" and os.environ.get("ACP_TEST_CHAT_SELECTORS"):
            assert value in ["balanced-52", "deep-93"] or (value == "extended-901" and self.current_model == self.model_b)
            self.thought = value
        elif config_id == "include-references" and os.environ.get("ACP_TEST_CHAT_SELECTORS"):
            assert isinstance(value, bool)
            self.references = value
        else:
            assert config_id == "detail-level" and value in ["brief", "full"]
            self.detail = value
        return s.SetSessionConfigOptionResponse(config_options=self.options())

    async def cancel(self, session_id, **kwargs):
        if not os.environ.get("ACP_TEST_IGNORE_CANCEL"):
            self.cancelled.setdefault(session_id, asyncio.Event()).set()

    async def prompt(self, session_id, prompt, **kwargs):
        command = "".join(getattr(block, "text", "") for block in prompt)
        if queue_log := os.environ.get("ACP_TEST_QUEUE_LOG"):
            with open(queue_log, "a", encoding="utf-8") as log:
                log.write("START:" + command + "\n")
        if command.startswith("queue-write:"):
            value = command.removeprefix("queue-write:")
            path = Path(self.cwd[session_id]) / "queue.txt"
            previous = path.read_text(encoding="utf-8") if path.exists() else ""
            path.write_bytes((previous + value + "\n").encode("utf-8"))
            return s.PromptResponse(stop_reason="end_turn")
        if os.environ.get("ACP_TEST_EMBEDDED_CONTEXT") and command.startswith("inspect attachments"):
            blocks = [block.model_dump(by_alias=True, exclude_none=True) for block in prompt]
            await self._send_agent_message(session_id, s.TextContentBlock(type="text", text=json.dumps(blocks, ensure_ascii=False)))
            return s.PromptResponse(stop_reason="end_turn")
        cancelled = self.cancelled[session_id] = asyncio.Event()
        if command == "background-error-gated":
            gates = Path(os.environ["ACP_TEST_CONTROL_GATE_DIR"])
            (gates / "prompt-ready").write_text("ready", encoding="utf-8")
            while not (gates / "fail-prompt").exists():
                if cancelled.is_set():
                    return s.PromptResponse(stop_reason="cancelled")
                await asyncio.sleep(0.01)
            self.edge_log("background-error", session_id)
            raise RequestError(-32000, "Gated background failure during history load")
        if command in ["live-changes-acp", "live-changes-shell"]:
            path = Path(self.cwd[session_id]) / "live.txt"
            gates = Path(os.environ["ACP_TEST_LIVE_GATE_DIR"])
            for stage, text, gate in [(1, "FIRST\nkeep\nlast\n", "next"),
                                      (2, "FIRST\nkeep\nLAST\n", "finish")]:
                if command == "live-changes-acp":
                    await self._conn.write_text_file(session_id, str(path), text)
                else:
                    subprocess.run([sys.executable, "-c",
                        "from pathlib import Path; import sys; Path(sys.argv[1]).write_bytes(sys.argv[2].encode('utf-8'))",
                        str(path), text], cwd=self.cwd[session_id], check=True,
                        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                (gates / f"ready-{stage}").write_text("written", encoding="utf-8")
                await self._send_agent_message(session_id, s.TextContentBlock(type="text", text=f"Write {stage} finished; waiting before the response ends"))
                while not (gates / gate).exists():
                    if cancelled.is_set():
                        return s.PromptResponse(stop_reason="cancelled")
                    await asyncio.sleep(0.01)
            return s.PromptResponse(stop_reason="end_turn")
        if command == "rollback-shell":
            # Write directly in a real child process, bypassing ACP fs methods.
            script = "from pathlib import Path; p=Path('shell.txt'); p.write_bytes(b'FIRST\\nkeep\\nLAST\\n'); Path('created.txt').write_bytes(b'created\\n')"
            subprocess.run([sys.executable, "-c", script], cwd=self.cwd[session_id], check=True,
                           stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            await self._send_agent_message(session_id, s.TextContentBlock(type="text", text="Shell edits finished"))
            return s.PromptResponse(stop_reason="end_turn")
        if command == "crash":
            print("intentional agent crash", file=sys.stderr, flush=True)
            os._exit(17)
        if command == "bad-json":
            os.write(1, b"this is not JSON\n")
            await asyncio.sleep(120)
        if command == "partial-json":
            os.write(1, b'{"jsonrpc":"2.0"')
            os._exit(18)
        if command == "error":
            raise RequestError(-32042, "A recoverable test error", {"retryable": True})
        if command in ["cancel", "silent"]:
            if command == "cancel":
                await self._send_agent_message(session_id, s.TextContentBlock(type="text", text="Working before cancellation"))
            else:
                print("Silent prompt received", file=sys.stderr, flush=True)
            await cancelled.wait()
            if queue_log := os.environ.get("ACP_TEST_QUEUE_LOG"):
                # Keep the old request alive after it receives cancellation.
                # Its last write must be captured before a successor is sent.
                await asyncio.sleep(0.15)
                (Path(self.cwd[session_id]) / "cancel-finished.txt").write_text("finished", encoding="utf-8")
                with open(queue_log, "a", encoding="utf-8") as log:
                    log.write("CANCEL_FINISHED\n")
            return s.PromptResponse(stop_reason="cancelled")
        if command == "flood":
            for _ in range(5000):
                await self._send_agent_message(session_id, s.TextContentBlock(type="text", text="stream "))
            return s.PromptResponse(stop_reason="end_turn")
        if command == "thinking-stream":
            # Gates let the workbench assert each partial frame before the
            # next chunk or final answer exists, without timing assumptions.
            for text, gate in [("First partial thought", "continue-thinking"),
                               ("; second partial thought", "finish-thinking")]:
                await self._conn.session_update(session_id, s.AgentThoughtChunk(
                    session_update="agent_thought_chunk", content=s.TextContentBlock(type="text", text=text)))
                while not (Path(self.cwd[session_id]) / gate).exists():
                    if cancelled.is_set():
                        return s.PromptResponse(stop_reason="cancelled")
                    await asyncio.sleep(0.01)
            await self._send_agent_message(session_id, s.TextContentBlock(type="text", text="Final answer after thinking"))
            return s.PromptResponse(stop_reason="end_turn")
        if command == "unknown-method":
            try:
                await self._conn.ext_method("_not_supported", {})
            except RequestError as error:
                await self._send_agent_message(session_id, s.TextContentBlock(type="text", text=f"unsupported: {error.code}"))
            return s.PromptResponse(stop_reason="end_turn")
        if command == "large-file":
            try:
                await self._conn.read_text_file(session_id, str(Path(self.cwd[session_id]) / "context.txt"))
                raise AssertionError("Expected an oversized response error")
            except RequestError as error:
                await self._send_agent_message(session_id, s.TextContentBlock(type="text", text=str(error)))
            return s.PromptResponse(stop_reason="end_turn")
        if command == "terminal-two":
            terminals = []
            for _ in range(2):
                terminals.append(await self._conn.create_terminal(session_id, sys.executable,
                    args=["-u", "-c", "import time; print('ready',flush=True); time.sleep(120)"], cwd=self.cwd[session_id]))
            for term in terminals:
                await self._conn.wait_for_terminal_exit(session_id, term.terminal_id)
                await self._conn.release_terminal(session_id, term.terminal_id)
            return s.PromptResponse(stop_reason="end_turn")
        if command in ["terminal-input", "terminal-kill", "terminal-release", "terminal-descendant", "terminal-failure", "terminal-history"]:
            return await self.terminal_scenario(session_id, command)
        if command not in ["exercise", "permission"]:
            return await super().prompt(session_id, prompt, **kwargs)

        await self._conn.session_update(session_id, s.AvailableCommandsUpdate(session_update="available_commands_update", available_commands=[s.AvailableCommand(name="inspect-local", description="Inspect this workspace")]))
        await self._conn.session_update(session_id, s.AgentThoughtChunk(session_update="agent_thought_chunk", content=s.TextContentBlock(type="text", text="Checking the workspace…")))
        await self._conn.session_update(session_id, s.AgentPlanUpdate(session_update="plan", entries=[s.PlanEntry(content="Update the fixture", priority="high", status="in_progress"), s.PlanEntry(content="Run a local command", priority="medium", status="pending")]))
        tool_id = "edit-" + session_id
        path = str(Path(self.cwd[session_id]) / "generated.txt")
        diff = s.FileEditToolCallContent(type="diff", path=path, old_text="before\n", new_text="after\n")
        await self._conn.session_update(session_id, s.ToolCallStart(session_update="tool_call", tool_call_id=tool_id, title="Update generated.txt", kind="edit", status="pending", content=[diff]))
        permission = await self._conn.request_permission(session_id=session_id,
            tool_call=s.ToolCallUpdate(tool_call_id=tool_id, title="Update generated.txt", content=[diff]),
            options=[s.PermissionOption(option_id="permit-local-37", name="Allow once", kind="allow_once"), s.PermissionOption(option_id="decline-local-91", name="Reject", kind="reject_once")])
        outcome = permission.outcome
        if outcome.outcome == "cancelled" or cancelled.is_set():
            return s.PromptResponse(stop_reason="cancelled")
        if outcome.option_id != "permit-local-37":
            await self._send_agent_message(session_id, s.TextContentBlock(type="text", text="Permission rejected"))
            return s.PromptResponse(stop_reason="end_turn")
        if command == "permission":
            await self._send_agent_message(session_id, s.TextContentBlock(type="text", text="Permission accepted"))
            return s.PromptResponse(stop_reason="end_turn")
        await self._conn.session_update(session_id, s.ToolCallProgress(session_update="tool_call_update", tool_call_id=tool_id, status="in_progress"))
        read = await self._conn.read_text_file(session_id=session_id, path=str(Path(self.cwd[session_id]) / "context.txt"), line=2, limit=1)
        await self._send_agent_message(session_id, s.TextContentBlock(type="text", text="Read: " + read.content))
        await self._conn.write_text_file(session_id=session_id, path=path, content="after\n")
        await self._conn.session_update(session_id, s.ToolCallProgress(session_update="tool_call_update", tool_call_id=tool_id, status="completed"))
        term = await self._conn.create_terminal(session_id=session_id, command=sys.executable,
            args=["-u", "-c", "import sys; sys.stdout.buffer.write(('line\\n'*40 + 'héllo 世界\\n').encode()); sys.stdout.flush(); sys.stderr.write('stderr-end\\n')"], cwd=self.cwd[session_id], output_byte_limit=96)
        await self._conn.session_update(session_id, s.ToolCallStart(session_update="tool_call", tool_call_id="run-"+session_id, title="Verify locally", kind="execute", status="in_progress", content=[s.TerminalToolCallContent(type="terminal", terminal_id=term.terminal_id)]))
        status = await self._conn.wait_for_terminal_exit(session_id, term.terminal_id)
        output = await self._conn.terminal_output(session_id, term.terminal_id)
        assert status.exit_code == 0 and output.truncated
        assert len(output.output.encode()) <= 96
        await self._conn.release_terminal(session_id, term.terminal_id)
        await self._conn.session_update(session_id, s.ToolCallProgress(session_update="tool_call_update", tool_call_id="run-"+session_id, status="completed"))
        await self._conn.session_update(session_id, s.AgentPlanUpdate(session_update="plan", entries=[s.PlanEntry(content="Update the fixture", priority="high", status="completed"), s.PlanEntry(content="Run a local command", priority="medium", status="completed")]))
        await self._conn.session_update(session_id, s.UsageUpdate(session_update="usage_update", used=256, size=8192))
        for chunk in ["Finished ", "with **streaming**", " and Unicode: 世界."]:
            await self._send_agent_message(session_id, s.TextContentBlock(type="text", text=chunk))
            await asyncio.sleep(0.01)
        return s.PromptResponse(stop_reason="end_turn")

    async def terminal_scenario(self, session_id, command):
        script = "import sys; print('ready',flush=True); print('input='+input(),flush=True)" if command == "terminal-input" else "import time; print('ready',flush=True); time.sleep(120)"
        if command == "terminal-descendant":
            script = "import subprocess,sys; child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(120)']); print('child='+str(child.pid),flush=True)"
        elif command == "terminal-failure":
            script = "import sys; print('command failed',flush=True); sys.exit(7)"
        elif command == "terminal-history":
            script = "import sys; [print(f'line-{i:03} 世界') for i in range(250)]; print('ready',flush=True); print('received='+input(),flush=True)"
        # Pipe-backed terminals on Windows otherwise inherit a legacy code page.
        # This fixture deliberately writes and receives Unicode through UTF-8.
        term = await self._conn.create_terminal(session_id, sys.executable, args=["-X", "utf8", "-u", "-c", script], cwd=self.cwd[session_id],
            output_byte_limit=64 if command == "terminal-history" else None)
        if command == "terminal-kill":
            await self._conn.kill_terminal(session_id, term.terminal_id)
        if command == "terminal-release":
            wait = asyncio.create_task(self._conn.wait_for_terminal_exit(session_id, term.terminal_id))
            await asyncio.sleep(0.05)
            await self._conn.release_terminal(session_id, term.terminal_id)
            try:
                await wait
            except RequestError:
                pass
        else:
            status = await self._conn.wait_for_terminal_exit(session_id, term.terminal_id)
            if command == "terminal-failure":
                assert status.exit_code == 7
            output = await self._conn.terminal_output(session_id, term.terminal_id)
            await self._send_agent_message(session_id, s.TextContentBlock(type="text", text=output.output))
            await self._conn.release_terminal(session_id, term.terminal_id)
        return s.PromptResponse(stop_reason="end_turn")


if __name__ == "__main__":
    # SDK 0.12.1 still gates session/close, despite exposing its schema.
    asyncio.run(run_agent(ScenarioAgent(), use_unstable_protocol=True))

"""An SDK peer that persists conversations and proves resumed context survives.

Used by the history regression tests and isolated workbench smoke tests. No
model, credentials, network, or editor-owned transcript storage is involved.
"""
import asyncio
import json
import os
from pathlib import Path
import uuid

from scenario_agent import ScenarioAgent, run_agent, s, RequestError


class HistoryAgent(ScenarioAgent):
    def __init__(self):
        super().__init__()
        self.storage = Path(os.environ["ACP_TEST_HISTORY_FILE"])
        self.history = json.loads(self.storage.read_text(encoding="utf-8")) if self.storage.exists() else {}
        self.fail_load_once = bool(os.environ.get("ACP_TEST_HISTORY_FAIL_LOAD_ONCE"))

    def save(self):
        temporary = self.storage.with_suffix(".tmp")
        temporary.write_text(json.dumps(self.history, ensure_ascii=False), encoding="utf-8")
        temporary.replace(self.storage)

    def record(self, method, session_id, **details):
        if path := os.environ.get("ACP_TEST_HISTORY_LOG"):
            with open(path, "a", encoding="utf-8") as log:
                log.write(json.dumps({"method": method, "sessionId": session_id, **details}) + "\n")

    async def new_session(self, cwd, **kwargs):
        session_id = "history-" + uuid.uuid4().hex
        self.history[session_id] = {"cwd": cwd, "messages": []}
        self.cwd[session_id] = cwd
        self._sessions.add(session_id)
        self.save()
        self.record("new", session_id)
        return s.NewSessionResponse(session_id=session_id, modes=self.modes(), config_options=self.options())

    async def list_sessions(self, cwd=None, cursor=None, **kwargs):
        sessions = [s.SessionInfo(session_id=session_id, cwd=chat["cwd"], title=chat["messages"][0]["text"])
                    for session_id, chat in self.history.items()
                    if chat["messages"] and (cwd is None or Path(chat["cwd"]) == Path(cwd))]
        self.record("list", None, sessions=[chat.session_id for chat in sessions])
        return s.ListSessionsResponse(sessions=sessions)

    def saved(self, session_id):
        if session_id not in self.history:
            raise RequestError(-32000, "Saved conversation does not exist")
        return self.history[session_id]

    async def load_session(self, cwd, session_id, **kwargs):
        chat = self.saved(session_id)
        self.record("load", session_id, messages=len(chat["messages"]))
        # A peer may replay messages before failing the request. A retry must
        # still perform session/load, rather than treating that partial view
        # as an already-open, continuable agent session.
        if self.fail_load_once:
            self.fail_load_once = False
            await self._send_agent_message(session_id, s.TextContentBlock(type="text", text="Incomplete replay"))
            self.record("load-failed", session_id)
            raise RequestError(-32000, "Temporary history load failure; try again")
        for message in chat["messages"]:
            content = s.TextContentBlock(type="text", text=message["text"])
            if message["role"] == "user":
                await self._conn.session_update(session_id, s.UserMessageChunk(session_update="user_message_chunk", content=content))
            else:
                await self._send_agent_message(session_id, content)
        self.cwd[session_id] = cwd
        self._sessions.add(session_id)
        return s.LoadSessionResponse(modes=self.modes(), config_options=self.options())

    async def resume_session(self, cwd, session_id, **kwargs):
        chat = self.saved(session_id)
        self.cwd[session_id] = cwd
        self._sessions.add(session_id)
        self.record("resume", session_id, messages=len(chat["messages"]))
        return s.ResumeSessionResponse(modes=self.modes(), config_options=self.options())

    async def prompt(self, session_id, prompt, **kwargs):
        if session_id not in self._sessions:
            self.record("unknown-prompt", session_id)
            raise RequestError(-32000, "Conversation must be loaded before continuing")
        chat = self.saved(session_id)
        text = "".join(getattr(block, "text", "") for block in prompt)
        previous = [message["text"] for message in chat["messages"] if message["role"] == "user"]
        turn = len(previous) + 1
        answer = f"Turn {turn}; first message: {previous[0] if previous else text}; session: {session_id}"
        chat["messages"].extend([{"role": "user", "text": text}, {"role": "agent", "text": answer}])
        self.save()
        self.record("prompt", session_id, text=text, turn=turn, first=previous[0] if previous else text)
        await self._send_agent_message(session_id, s.TextContentBlock(type="text", text=answer))
        return s.PromptResponse(stop_reason="end_turn")

    async def delete_session(self, session_id, **kwargs):
        self.history.pop(session_id, None)
        self._sessions.discard(session_id)
        self.save()
        self.record("delete", session_id)
        return s.DeleteSessionResponse()


if __name__ == "__main__":
    asyncio.run(run_agent(HistoryAgent(), use_unstable_protocol=True))

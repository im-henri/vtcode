"""An isolated interactive-job hop for terminal bridge regression tests.

Deliberately has no bridge knowledge: it allocates a job PTY, starts an arbitrary
command, relays bytes and window changes, and propagates exit status.
"""
import errno
import fcntl
import json
import os
import pty
import select
import signal
import sys
import termios
import tty


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: bridge_job.py command [arguments ...]")
    command = sys.argv[1:]
    # Optional faults live inside this otherwise transparent job hop. The
    # production bridge never sees or interprets these test-only options.
    options = os.environ.pop("VTCODE_BRIDGE_LAB_OPTIONS", None)
    if options is not None:
        lab = os.path.join(os.path.dirname(__file__), "../../bridge_lab.py")
        command = [sys.executable, lab, *json.loads(options), "--", *command]
    saved = termios.tcgetattr(0)
    pid, master = pty.fork()
    if pid == 0:
        os.execvp(command[0], command)

    def resize(*_):
        size = fcntl.ioctl(0, termios.TIOCGWINSZ, bytes(8))
        fcntl.ioctl(master, termios.TIOCSWINSZ, size)

    signal.signal(signal.SIGWINCH, resize)
    tty.setraw(0)
    resize()
    try:
        while True:
            for source in select.select([0, master], [], [])[0]:
                try:
                    data = os.read(source, 16 * 1024)
                except OSError as error:
                    if source == master and error.errno == errno.EIO:
                        return os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1])
                    raise
                if not data:
                    return os.waitstatus_to_exitcode(os.waitpid(pid, 0)[1])
                target = master if source == 0 else 1
                while data:
                    data = data[os.write(target, data):]
    finally:
        termios.tcsetattr(0, termios.TCSANOW, saved)
        os.close(master)


if __name__ == "__main__":
    raise SystemExit(main())

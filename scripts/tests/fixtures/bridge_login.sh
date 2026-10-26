# Sourced by an isolated interactive login-shell test. The alias is deliberately
# opaque to the local bridge, just like a site's scheduler wrapper or alias.
fixture_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
alias fixture_submit="python3 '$fixture_dir/bridge_job.py'"
job_root=$(mktemp -d)
trap 'rm -rf -- "$job_root"' EXIT
printf 'fn example() { }\n' > "$job_root/remote.rs"
cp -- "$fixture_dir/../../../tests/fixtures/remote_agent_peer.py" "$job_root/agent-peer.py"
printf 'disk one\ndisk two\n' > "$job_root/agent.txt"
git init --quiet "$job_root"
if [[ ${2:-} == lab ]]; then
    export VTCODE_BRIDGE_LAB_OPTIONS="[\"--latency-ms\",\"40\",\"--jitter-ms\",\"10\",\"--bytes-per-second\",\"32768\",\"--single-core\",\"--cpu-load-percent\",\"70\",\"--disk-delay-ms\",\"20\",\"--disk-root\",\"$job_root\",\"--timeout\",\"120\"]"
fi
cd -- "$job_root"
fixture_submit "$1" --exact remote::tests::headless_service_fixture --ignored --nocapture

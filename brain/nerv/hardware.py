"""Hardware and Environment telemetry for auto-optimizing local LLM setup.

Nerv dynamically sniffs the host machine and adjusts options such as
suggested model tiers, concurrency limits, and Ollama context sizes
based on available RAM and CPU/GPU architectures.
"""

import json
import logging
import os
import platform
import shutil
import subprocess
from typing import TypedDict

import httpx

logger = logging.getLogger(__name__)

DEFAULT_OLLAMA_URL = "http://localhost:11434"
OLLAMA_DISCOVERY_TIMEOUT = 5.0


class HardwareProfile(TypedDict):
    os_name: str
    arch: str
    ram_gb: int
    has_apple_silicon: bool
    recommended_router_model: str
    ollama_options: dict


def _get_total_ram_gb() -> int:
    """Get the total system RAM in GB."""
    try:
        if platform.system() == "Darwin":
            output = subprocess.check_output(["sysctl", "-n", "hw.memsize"])
            return int(output.strip()) // (1024**3)
        elif platform.system() == "Linux":
            with open("/proc/meminfo") as f:
                for line in f:
                    if "MemTotal" in line:
                        kb = int(line.split()[1])
                        return kb // (1024**2)
    except Exception as e:
        logger.warning(f"Could not determine system RAM: {e}")
    return 8  # Fallback to conservative 8GB assumption


def _is_apple_silicon() -> bool:
    """Check if the system is running on Apple M-series chips."""
    if platform.system() != "Darwin":
        return False
    try:
        output = subprocess.check_output(
            ["sysctl", "-n", "machdep.cpu.brand_string"], text=True
        )
        return "Apple" in output
    except Exception:
        return False


def get_hardware_profile() -> HardwareProfile:
    """Sniff the hardware environment and return optimal configurations."""
    os_name = platform.system()
    arch = platform.machine()
    ram_gb = _get_total_ram_gb()
    is_m_chip = _is_apple_silicon()

    # 1. Determine recommended router model based on RAM
    if ram_gb >= 32:
        recommended_model = "qwen2.5:3b"
    else:
        recommended_model = "qwen2.5:1.5b"

    # 2. Determine Ollama runner options
    ollama_options = {}

    # Optimization: on Apple Silicon, limit threads to avoid performance core starvation
    if is_m_chip:
        ollama_options["num_thread"] = 8 if ram_gb >= 16 else 4

    # Standardize options
    ollama_options["num_ctx"] = 4096 if ram_gb >= 16 else 2048

    logger.info(
        "Hardware Sniff: OS=%s Arch=%s RAM=%dGB AppleSilicon=%s",
        os_name,
        arch,
        ram_gb,
        is_m_chip,
    )
    logger.info(
        "Optimal Router Model: %s, Options: %s", recommended_model, ollama_options
    )

    return {
        "os_name": os_name,
        "arch": arch,
        "ram_gb": ram_gb,
        "has_apple_silicon": is_m_chip,
        "recommended_router_model": recommended_model,
        "ollama_options": ollama_options,
    }


def _get_ollama_base_url(base_url: str | None = None) -> str:
    """Resolve the Ollama base URL from args or environment."""
    return (base_url or os.environ.get("NERV_OLLAMA_URL", DEFAULT_OLLAMA_URL)).rstrip(
        "/"
    )


def _check_model_via_http(model_name: str, base_url: str) -> bool | None:
    """Return model presence via Ollama HTTP API, or None if the server is unreachable."""
    try:
        with httpx.Client(timeout=OLLAMA_DISCOVERY_TIMEOUT) as client:
            response = client.get(f"{base_url}/api/tags")
            response.raise_for_status()
    except httpx.HTTPError as e:
        logger.warning("Ollama API not reachable at %s: %s", base_url, e)
        return None

    models = response.json().get("models", [])
    for model in models:
        model_id = model.get("model") or model.get("name")
        if model_id == model_name:
            return True

    return False


def _pull_model_via_http(model_name: str, base_url: str) -> bool:
    """Pull a model via Ollama HTTP API."""
    try:
        with (
            httpx.Client(timeout=None) as client,
            client.stream(
                "POST",
                f"{base_url}/api/pull",
                json={"model": model_name},
            ) as response,
        ):
            response.raise_for_status()

            last_status = ""
            last_progress = -1
            for line in response.iter_lines():
                if not line:
                    continue

                payload = json.loads(line)
                status = payload.get("status", "").strip()
                completed = payload.get("completed")
                total = payload.get("total")

                if isinstance(completed, int) and isinstance(total, int) and total > 0:
                    progress = int(completed * 100 / total)
                    if status != last_status or progress >= last_progress + 10:
                        logger.info(
                            "Pulling %s via Ollama API: %s (%d%%)",
                            model_name,
                            status or "downloading",
                            progress,
                        )
                        last_status = status
                        last_progress = progress
                elif status and status != last_status:
                    logger.info("Pulling %s via Ollama API: %s", model_name, status)
                    last_status = status
    except httpx.HTTPError as e:
        logger.error("Failed to auto-pull model %s via Ollama API: %s", model_name, e)
        return False
    except json.JSONDecodeError as e:
        logger.error("Invalid Ollama pull progress for model %s: %s", model_name, e)
        return False

    logger.info("Successfully pulled model via Ollama API: %s", model_name)
    return True


def _check_and_pull_model_via_cli(model_name: str) -> bool:
    """Check if a model exists via CLI and pull it if missing."""
    try:
        output = subprocess.check_output(["ollama", "list"], text=True)
        if model_name in output:
            return True

        logger.info(
            "Model '%s' not found locally. Initiating auto-pull via Ollama CLI...",
            model_name,
        )
        subprocess.run(
            ["ollama", "pull", model_name],
            stdout=subprocess.DEVNULL,
            stderr=None,
            check=True,
        )
        logger.info("Successfully pulled model via Ollama CLI: %s", model_name)
        return True
    except subprocess.CalledProcessError as e:
        logger.error("Failed to auto-pull model %s via Ollama CLI: %s", model_name, e)
        return False


def check_and_pull_model(model_name: str, base_url: str | None = None) -> None:
    """Check if an Ollama model exists and auto-pull it when the server is available."""
    resolved_base_url = _get_ollama_base_url(base_url)
    http_status = _check_model_via_http(model_name, resolved_base_url)

    if http_status is True:
        return

    if http_status is False:
        logger.info(
            "Model '%s' not found on Ollama server %s. Initiating auto-pull...",
            model_name,
            resolved_base_url,
        )
        if _pull_model_via_http(model_name, resolved_base_url):
            return

    if shutil.which("ollama") is None:
        logger.error(
            "Ollama is not installed or not running. Nerv can auto-pull missing models, "
            "but it cannot install Ollama itself. Install/start Ollama first: "
            "https://ollama.com"
        )
        return

    _check_and_pull_model_via_cli(model_name)


# Compute globally once at startup
PROFILE = get_hardware_profile()

"""Hardware and Environment telemetry for auto-optimizing local LLM setup.

Nerv dynamically sniffs the host machine and adjusts options such as
suggested model tiers, concurrency limits, and Ollama context sizes
based on available RAM and CPU/GPU architectures.
"""

import logging
import platform
import subprocess
import shutil
from typing import TypedDict

logger = logging.getLogger(__name__)

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
            return int(output.strip()) // (1024 ** 3)
        elif platform.system() == "Linux":
            with open("/proc/meminfo", "r") as f:
                for line in f:
                    if "MemTotal" in line:
                        kb = int(line.split()[1])
                        return kb // (1024 ** 2)
    except Exception as e:
        logger.warning(f"Could not determine system RAM: {e}")
    return 8  # Fallback to conservative 8GB assumption

def _is_apple_silicon() -> bool:
    """Check if the system is running on Apple M-series chips."""
    if platform.system() != "Darwin":
        return False
    try:
        output = subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"], text=True)
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
        recommended_model = "qwen2.5:7b"
    elif ram_gb >= 16:
        recommended_model = "qwen2.5:3b"
    else:
        recommended_model = "llama3.2:1b"
        
    # 2. Determine Ollama runner options
    ollama_options = {}
    
    # Optimization: on Apple Silicon, limit threads to avoid performance core starvation
    if is_m_chip:
        ollama_options["num_thread"] = 8 if ram_gb >= 16 else 4
        
    # Standardize options
    ollama_options["num_ctx"] = 4096 if ram_gb >= 16 else 2048
    
    logger.info("Hardware Sniff: OS=%s Arch=%s RAM=%dGB AppleSilicon=%s", os_name, arch, ram_gb, is_m_chip)
    logger.info("Optimal Router Model: %s, Options: %s", recommended_model, ollama_options)
    
    return {
        "os_name": os_name,
        "arch": arch,
        "ram_gb": ram_gb,
        "has_apple_silicon": is_m_chip,
        "recommended_router_model": recommended_model,
        "ollama_options": ollama_options
    }

# Compute globally once at startup
PROFILE = get_hardware_profile()

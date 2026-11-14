"""Strip credential-shaped variables from environments handed to child processes.

The brain loads ``personal/api_keys.env`` into its own environment, so any child
that inherits it wholesale (a confirmed ``shell`` command, a desktop helper) can
read every provider key. Children get a scrubbed copy instead.
"""

import os
import re
from collections.abc import Iterable, Mapping

# Names containing any of these words are treated as credentials. KEY, TOKEN,
# SECRET and PASSWORD are the core set; PASSWD, CREDENTIAL (AWS-style) and
# AUTH (e.g. SSH_AUTH_SOCK, HTTP_AUTH) are added because they carry the same
# kind of access. Matching is case-insensitive and substring-based, so it
# over-matches (e.g. KEYBOARD_LAYOUT); users restore those via passthrough.
SENSITIVE_ENV_PATTERN = re.compile(
    r"KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|AUTH", re.IGNORECASE
)


def scrubbed_env(
    environ: Mapping[str, str] | None = None,
    passthrough: Iterable[str] = (),
) -> dict[str, str]:
    """Return a copy of ``environ`` without credential-shaped variables.

    ``environ`` defaults to ``os.environ`` and is never modified. Names listed
    in ``passthrough`` (exact, case-sensitive) are kept even if they match.
    """
    source = os.environ if environ is None else environ
    allowed = set(passthrough)
    return {
        name: value
        for name, value in source.items()
        if name in allowed or not SENSITIVE_ENV_PATTERN.search(name)
    }

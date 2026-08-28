"""Tests for Ollama discovery and model auto-pull behavior."""

from unittest.mock import patch

from nerv import hardware


class TestCheckAndPullModel:
    """Verify model bootstrap behavior across Ollama discovery paths."""

    def test_uses_http_pull_when_server_is_reachable(self) -> None:
        with (
            patch.object(
                hardware, "_check_model_via_http", return_value=False
            ) as mock_check,
            patch.object(
                hardware, "_pull_model_via_http", return_value=True
            ) as mock_pull,
            patch("shutil.which", return_value=None),
            patch("subprocess.check_output") as mock_check_output,
        ):
            hardware.check_and_pull_model("qwen2.5:1.5b", "http://localhost:11434")

        mock_check.assert_called_once_with("qwen2.5:1.5b", "http://localhost:11434")
        mock_pull.assert_called_once_with("qwen2.5:1.5b", "http://localhost:11434")
        mock_check_output.assert_not_called()

    def test_logs_clear_error_when_server_unreachable_and_cli_missing(
        self, caplog
    ) -> None:
        with (
            patch.object(hardware, "_check_model_via_http", return_value=None),
            patch("shutil.which", return_value=None),
        ):
            hardware.check_and_pull_model("qwen2.5:1.5b", "http://localhost:11434")

        assert "cannot install Ollama itself" in caplog.text

    def test_falls_back_to_cli_when_http_is_unreachable(self) -> None:
        with (
            patch.object(hardware, "_check_model_via_http", return_value=None),
            patch("shutil.which", return_value="/usr/local/bin/ollama"),
            patch.object(
                hardware, "_check_and_pull_model_via_cli", return_value=True
            ) as mock_cli,
        ):
            hardware.check_and_pull_model("qwen2.5:1.5b", "http://localhost:11434")

        mock_cli.assert_called_once_with("qwen2.5:1.5b")

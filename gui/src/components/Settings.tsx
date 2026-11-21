import { useEffect, useState } from "react";
import {
  exportData,
  getConfigYaml,
  importData,
  ollamaDelete,
  ollamaModels,
  ollamaPull,
  saveConfigYaml,
} from "../api";

/// The settings page (PRD 4.1 #5): heartbeat/proactive config, data
/// export, and the advanced raw config.yaml editor (parse-validated save).
export function Settings() {
  const [configYaml, setConfigYaml] = useState<string>("");
  const [configStatus, setConfigStatus] = useState<string>("");
  const [configLoading, setConfigLoading] = useState(true);
  const [exportStatus, setExportStatus] = useState<string>("");
  const [importPath, setImportPath] = useState<string>("");
  const [importStatus, setImportStatus] = useState<string>("");
  const [installed, setInstalled] = useState<string[]>([]);
  const [pullName, setPullName] = useState<string>("");
  const [modelStatus, setModelStatus] = useState<string>("");

  useEffect(() => {
    getConfigYaml()
      .then(setConfigYaml)
      .catch((e) => setConfigStatus(`⚠️ ${String(e)}`))
      .finally(() => setConfigLoading(false));
    refreshModels();
  }, []);

  function refreshModels() {
    ollamaModels()
      .then(setInstalled)
      .catch(() => setInstalled([]));
  }

  async function pull(name: string) {
    if (!name.trim()) return;
    setModelStatus(`Downloading ${name}…`);
    try {
      await ollamaPull(name);
      setModelStatus(`✓ ${name} ready.`);
      refreshModels();
    } catch (e) {
      setModelStatus(`⚠️ ${String(e)}`);
    }
  }

  async function remove(name: string) {
    try {
      await ollamaDelete(name);
      setModelStatus(`✓ ${name} deleted.`);
      refreshModels();
    } catch (e) {
      setModelStatus(`⚠️ ${String(e)}`);
    }
  }

  async function save() {
    setConfigStatus("");
    try {
      await saveConfigYaml(configYaml);
      setConfigStatus("✓ Saved and validated (takes effect on gateway restart).");
    } catch (e) {
      setConfigStatus(`⚠️ ${String(e)}`);
    }
  }

  async function doExport() {
    try {
      setExportStatus(await exportData());
    } catch (e) {
      setExportStatus(`⚠️ ${String(e)}`);
    }
  }

  return (
    <div className="settings">
      <div className="settings-section">
        <div className="panel-title">Data</div>
        <button onClick={() => void doExport()}>Export personal data</button>
        <div className="wizard-hint">
          Bundles personal/ (agents, memory, facts, workflows, skills,
          calendar) into a timestamped JSON archive under personal/exports/.
        </div>
        {exportStatus && <div className="editor-status">{exportStatus}</div>}
      </div>

      <div className="settings-section">
        <div className="panel-title">Local models (Ollama)</div>
        {installed.length === 0 && (
          <div className="panel-empty">No models installed.</div>
        )}
        {installed.map((name) => (
          <div className="model-row" key={name}>
            <span>{name}</span>
            <button className="model-delete" onClick={() => void remove(name)}>
              Delete
            </button>
          </div>
        ))}
        <div className="wizard-field">
          <span>Pull a model by name</span>
          <input
            value={pullName}
            placeholder="qwen2.5:7b"
            onChange={(e) => setPullName(e.target.value)}
          />
          <button onClick={() => void pull(pullName)}>Pull</button>
        </div>
        {modelStatus && <div className="editor-status">{modelStatus}</div>}
      </div>

      <div className="settings-section">
        <div className="panel-title">Import</div>
        <div className="wizard-hint">
          Restore a nerv-export-*.json archive. Every file in the archive
          overwrites the matching personal/ path — point it at your own
          archive only.
        </div>
        <div className="wizard-field">
          <span>Archive path</span>
          <input
            value={importPath}
            placeholder="personal/exports/nerv-export-20261121-120000.json"
            onChange={(e) => setImportPath(e.target.value)}
          />
          <button
            onClick={() =>
              void importData(importPath)
                .then(setImportStatus)
                .catch((e: unknown) => setImportStatus(`⚠️ ${String(e)}`))
            }
            disabled={importPath.trim() === ""}
          >
            Import
          </button>
        </div>
        {importStatus && <div className="editor-status">{importStatus}</div>}
      </div>

      <div className="settings-section">
        <div className="panel-title">Config.yaml (advanced)</div>
        <div className="wizard-hint">
          The user config overrides core/config.default.yaml. Saving is
          parse-validated against the schema; the gateway picks it up on
          restart. Holds tokens — mode is forced to 0600.
        </div>
        {configLoading ? (
          <div className="panel-empty">Loading…</div>
        ) : (
          <>
            <textarea
              className="editor-yaml"
              value={configYaml || "# Empty — everything is default.\n"}
              spellCheck={false}
              onChange={(e) => setConfigYaml(e.target.value)}
            />
            <div className="editor-actions">
              <button onClick={() => void save()}>Validate & save</button>
              <span className="editor-status">{configStatus}</span>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

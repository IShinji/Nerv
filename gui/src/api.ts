// Thin wrapper over Tauri commands exposed by the Rust backend (src-tauri).
import { invoke } from "@tauri-apps/api/core";

export interface Agent {
  name: string;
  description: string;
}

export interface RequestRecord {
  tier: number | null;
  model: string;
  provider: string;
  est_input_tokens: number;
  cost_usd: number | null;
  duration_ms: number;
  error: string | null;
}

export interface SystemStats {
  requests: number;
  est_input_tokens: number;
  cost_usd: number;
  recent: RequestRecord[];
}

export interface ReplyMeta {
  reply: string;
  model?: string;
  provider?: string;
  est_input_tokens?: number;
  cost_usd?: number | null;
  duration_ms?: number;
  intent?: string;
}

export async function sendMessage(text: string): Promise<ReplyMeta> {
  return invoke<ReplyMeta>("send_message", { text });
}

/// Install an agent YAML from a URL (Agent Store client capability).
export async function installAgent(url: string, name?: string): Promise<string> {
  return invoke<string>("install_agent", { url, name: name ?? "" });
}

export interface FilePayload {
  name: string;
  text: string | null;
  size_bytes: number;
  truncated: boolean;
}

export async function readDroppedFile(path: string): Promise<FilePayload> {
  return invoke<FilePayload>("read_dropped_file", { path });
}

export interface BreakdownBucket {
  requests: number;
  est_input_tokens: number;
  cost_usd: number;
  errors: number;
}

export interface Breakdown {
  per_day: Record<string, BreakdownBucket>;
  per_model: Record<string, BreakdownBucket>;
}

export async function statsBreakdown(): Promise<Breakdown> {
  return invoke<Breakdown>("stats_breakdown");
}

export async function getConfigYaml(): Promise<string> {
  return invoke<string>("get_config_yaml");
}

export async function saveConfigYaml(yaml: string): Promise<void> {
  return invoke<void>("save_config_yaml", { yaml });
}

export async function exportData(): Promise<string> {
  return invoke<string>("export_data");
}

export async function importData(archive: string): Promise<string> {
  return invoke<string>("import_data", { archive });
}

export async function listAgents(): Promise<Agent[]> {
  return invoke<Agent[]>("list_agents");
}

export async function checkHeartbeat(): Promise<string> {
  return invoke<string>("check_heartbeat");
}

export async function getAgent(name: string): Promise<string> {
  return invoke<string>("get_agent", { name });
}

export async function saveAgent(name: string, yaml: string): Promise<void> {
  return invoke<void>("save_agent", { name, yaml });
}

export async function systemStats(): Promise<SystemStats> {
  return invoke<SystemStats>("system_stats");
}

export interface HardwareProfile {
  os_name: string;
  arch: string;
  ram_gb: number;
  has_apple_silicon: boolean;
  recommended_router_model: string;
}

export async function hardwareProfile(): Promise<HardwareProfile> {
  return invoke<HardwareProfile>("hardware_profile");
}

export async function ollamaModels(): Promise<string[]> {
  return invoke<string[]>("ollama_models");
}

export async function ollamaPull(model: string): Promise<void> {
  return invoke<void>("ollama_pull", { model });
}

export interface MemoryStats {
  total_ram_gb: number;
  used_ram_gb: number;
  cpu_percent: number;
}

export async function memoryStats(): Promise<MemoryStats> {
  return invoke<MemoryStats>("memory_stats");
}

export async function ollamaLoaded(): Promise<string[]> {
  return invoke<string[]>("ollama_loaded");
}

export interface TaskRow {
  id: string;
  kind: string;
  status: string;
}

export async function taskQueue(): Promise<TaskRow[]> {
  return invoke<TaskRow[]>("task_queue");
}

export async function saveApiKeys(keys: Record<string, string>): Promise<void> {
  return invoke<void>("save_api_keys", { keys });
}

export async function saveChannels(channels: Record<string, unknown>): Promise<void> {
  return invoke<void>("save_channels", { channels });
}

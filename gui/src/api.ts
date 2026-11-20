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

export async function sendMessage(text: string): Promise<string> {
  return invoke<string>("send_message", { text });
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

// Thin wrapper over Tauri commands exposed by the Rust backend (src-tauri).
import { invoke } from "@tauri-apps/api/core";

export interface Agent {
  name: string;
  description: string;
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

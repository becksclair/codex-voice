/** Browser configuration used for provider-direct streaming plus server-job fallback. */
export interface BrowserGoogleConfig {
  models: string[];
  voice?: string;
  timeoutMs: number;
}

export interface BrowserElevenLabsConfig {
  apiKey: string;
  baseUrl: string;
  models: string[];
  applyTextNormalization: string;
  streamGain: number;
  languageCode?: string;
  timeoutMs: number;
}

export interface BrowserProviders {
  google?: BrowserGoogleConfig;
  elevenlabs?: BrowserElevenLabsConfig;
}

export interface BrowserSpeechPrepConfig {
  mode: "performance-tags" | "shorten";
  model: string;
}

export interface BrowserPersonaConfig {
  label: string;
  description: string;
  provider: string;
  providerOrder: string[];
  google?: {
    voiceName: string;
  };
  elevenlabs?: {
    voiceId: string;
    voiceSettings: {
      stability: number;
      similarityBoost: number;
      style: number;
      useSpeakerBoost: boolean;
      speed: number;
    };
  };
}

export interface BrowserTtsConfig {
  version: 2;
  defaultProvider: string;
  defaultPersona?: string;
  providers: BrowserProviders;
  speechPrep?: BrowserSpeechPrepConfig;
  personas: Record<string, BrowserPersonaConfig>;
}

export async function fetchConfig(): Promise<BrowserTtsConfig> {
  const response = await fetch("/web/config", { cache: "no-store" });
  if (!response.ok) {
    throw new Error(
      response.status === 503
        ? "Speech backend is not configured."
        : `Speech backend configuration failed (${response.status}).`,
    );
  }
  const config = (await response.json()) as BrowserTtsConfig;
  if (config?.version !== 2 || !config.providers || !config.personas) {
    throw new Error("Speech backend returned an unsupported configuration.");
  }
  return config;
}

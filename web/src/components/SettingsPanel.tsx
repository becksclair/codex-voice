import type { SelectOption, SettingsState } from "../hooks/useSettings.ts";
import type { ThemePreference } from "../lib/index.ts";

interface SettingsPanelProps {
  open: boolean;
  settings: SettingsState;
}

function Options({ options }: { options: SelectOption[] }) {
  return (
    <>
      {options.map((option) => (
        <option key={option.value} value={option.value}>
          {option.label}
        </option>
      ))}
    </>
  );
}

/**
 * The settings drawer (`#settings-panel`): provider/voice/model/theme selects and
 * the emotion/summarize/generate-on-paste toggles.
 */
export function SettingsPanel({ open, settings }: SettingsPanelProps) {
  return (
    <div className="settings" id="settings-panel" hidden={!open}>
      <div className="settings-grid">
        <label className="field">
          Provider
          <select
            id="provider"
            value={settings.provider}
            onChange={(event) => settings.setProvider(event.target.value)}
          >
            <Options options={settings.providerOptions} />
          </select>
        </label>
        <label className="field">
          Voice
          <select
            id="voice"
            value={settings.voice}
            onChange={(event) => settings.setVoice(event.target.value)}
          >
            <Options options={settings.voiceOptions} />
          </select>
        </label>
        <label className="field">
          Model
          <select
            id="model"
            value={settings.model}
            onChange={(event) => settings.setModel(event.target.value)}
          >
            <Options options={settings.modelOptions} />
          </select>
        </label>
        <label className="field">
          Theme
          <select
            id="theme"
            value={settings.settings.theme || "auto"}
            onChange={(event) => settings.setTheme(event.target.value as ThemePreference)}
          >
            <option value="auto">Auto</option>
            <option value="dark">Dark</option>
            <option value="light">Light</option>
          </select>
        </label>
        <div className="toggles">
          <label className="toggle">
            <input
              id="emotion"
              type="checkbox"
              checked={settings.settings.emotionPreprocessing}
              onChange={(event) => settings.setEmotion(event.target.checked)}
            />
            Emotion
          </label>
          <label className="toggle">
            <input
              id="summarize"
              type="checkbox"
              checked={settings.settings.summarization}
              onChange={(event) => settings.setSummarize(event.target.checked)}
            />
            Summarize
          </label>
          <label className="toggle">
            <input
              id="generate-on-paste"
              type="checkbox"
              checked={settings.settings.generateOnPaste !== false}
              onChange={(event) => settings.setGenerateOnPaste(event.target.checked)}
            />
            Generate on paste
          </label>
        </div>
      </div>
    </div>
  );
}

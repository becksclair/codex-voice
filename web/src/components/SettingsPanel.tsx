import type { SelectOption, SettingsState } from "../hooks/useSettings.ts";
import type { ThemePreference } from "../lib/index.ts";

interface SettingsPanelProps {
  open: boolean;
  settings: SettingsState;
  generationBusy?: boolean;
}

const FIELD = "grid gap-1.5 text-[0.9rem] text-[var(--muted)]";
const SELECT =
  "min-h-[42px] w-full rounded-2xl border border-[var(--line)] bg-[var(--panel-soft)] px-2.5 text-[var(--text)] disabled:cursor-not-allowed disabled:opacity-55";
const TOGGLE =
  "grid min-h-[42px] gap-1 rounded-2xl border border-[var(--line)] bg-[var(--panel-soft)] px-2.5 py-2 font-[650] text-[var(--text)] has-[:disabled]:cursor-not-allowed has-[:disabled]:opacity-55";
const CHECKBOX = "h-[18px] w-[18px] [accent-color:var(--mint)]";

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
export function SettingsPanel({ open, settings, generationBusy = false }: SettingsPanelProps) {
  return (
    <div
      id="settings-panel"
      hidden={!open}
      className="rounded-[22px] border border-[var(--line)] bg-[var(--settings-bg)] shadow-[var(--settings-shadow)]"
    >
      <div className="grid gap-3 p-3.5">
        <label className={FIELD}>
          Provider
          <select
            id="provider"
            className={SELECT}
            value={settings.provider}
            disabled={generationBusy}
            onChange={(event) => settings.setProvider(event.target.value)}
          >
            <Options options={settings.providerOptions} />
          </select>
        </label>
        <label className={FIELD}>
          Voice
          <select
            id="voice"
            className={SELECT}
            value={settings.voice}
            disabled={generationBusy}
            onChange={(event) => settings.setVoice(event.target.value)}
          >
            <Options options={settings.voiceOptions} />
          </select>
        </label>
        <label className={FIELD}>
          Model
          <select
            id="model"
            className={SELECT}
            value={settings.model}
            disabled={generationBusy}
            onChange={(event) => settings.setModel(event.target.value)}
          >
            <Options options={settings.modelOptions} />
          </select>
        </label>
        <label className={FIELD}>
          Theme
          <select
            id="theme"
            className={SELECT}
            value={settings.settings.theme || "auto"}
            onChange={(event) => settings.setTheme(event.target.value as ThemePreference)}
          >
            <option value="auto">Auto</option>
            <option value="dark">Dark</option>
            <option value="light">Light</option>
          </select>
        </label>
        <div className="grid grid-cols-2 gap-2 max-[420px]:grid-cols-1">
          <label className={TOGGLE}>
            <span className="flex items-center gap-2">
              <input
                id="emotion"
                type="checkbox"
                className={CHECKBOX}
                checked={settings.settings.emotionPreprocessing}
                disabled={generationBusy}
                aria-describedby="emotion-help"
                onChange={(event) => settings.setEmotion(event.target.checked)}
              />
              Emotion
            </span>
            <span
              id="emotion-help"
              className="text-[0.76rem] leading-snug font-normal text-[var(--muted)]"
            >
              Adds model-supported delivery cues without changing your wording.
            </span>
          </label>
          <label className={TOGGLE}>
            <span className="flex items-center gap-2">
              <input
                id="summarize"
                type="checkbox"
                className={CHECKBOX}
                checked={settings.settings.summarization}
                disabled={generationBusy}
                aria-describedby="summarize-help"
                onChange={(event) => settings.setSummarize(event.target.checked)}
              />
              Summarize
            </span>
            <span
              id="summarize-help"
              className="text-[0.76rem] leading-snug font-normal text-[var(--muted)]"
            >
              Shortens only when the text exceeds the selected voice’s limit.
            </span>
          </label>
          <label className={TOGGLE}>
            <input
              id="generate-on-paste"
              type="checkbox"
              className={CHECKBOX}
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

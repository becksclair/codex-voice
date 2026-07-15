import { render } from "@testing-library/react";
import { expect, test } from "vitest";
import { GenerateBar } from "./GenerateBar.tsx";

const baseProps = {
  generating: false,
  generationActive: false,
  progress: 0,
  label: "Generate",
  onGenerate: () => {},
  paused: true,
  playDisabled: true,
  onTogglePlay: () => {},
  downloadDisabled: true,
  onDownload: () => {},
  settingsOpen: false,
  onToggleSettings: () => {},
};

test("generate button reflects the busy/label/progress props", () => {
  const { rerender } = render(<GenerateBar {...baseProps} />);
  const generate = document.getElementById("generate") as HTMLButtonElement;
  const label = document.getElementById("generate-label") as HTMLElement;
  expect(generate.disabled).toBe(false);
  expect(generate.classList.contains("generating")).toBe(false);
  expect(label.textContent).toBe("Generate");

  rerender(
    <GenerateBar
      {...baseProps}
      generating={true}
      generationActive={true}
      label="Starting"
      progress={0.08}
    />,
  );
  expect(generate.disabled).toBe(false);
  expect(generate.classList.contains("generating")).toBe(true);
  expect(label.children[0]?.textContent).toBe("Generating...");
  expect(label.children[1]?.textContent).toBe("Tap to Stop");
  expect(label.classList.contains("text-center")).toBe(true);
  expect(generate.getAttribute("aria-label")).toBe("Stop generation");
  expect(generate.style.getPropertyValue("--generate-progress")).toBe("0.08");
});

test("play button toggles icon and aria-label with the paused prop", () => {
  const { rerender } = render(<GenerateBar {...baseProps} paused={true} />);
  const play = document.getElementById("play") as HTMLButtonElement;
  const icon = document.getElementById("play-icon") as unknown as SVGSVGElement;
  expect(play.getAttribute("aria-label")).toBe("Play");
  // Paused: single "play" triangle path.
  expect(icon.querySelectorAll("path").length).toBe(1);

  rerender(<GenerateBar {...baseProps} paused={false} />);
  expect(play.getAttribute("aria-label")).toBe("Pause");
  // Playing: two "pause" bar paths.
  expect(icon.querySelectorAll("path").length).toBe(2);
});

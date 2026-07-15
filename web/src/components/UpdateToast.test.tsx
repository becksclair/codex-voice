import { act, fireEvent, render } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { UPDATE_TOAST_DURATION_MS, UpdateToast } from "./UpdateToast.tsx";

afterEach(() => vi.useRealTimers());

test("dismisses immediately from its button", () => {
  render(<UpdateToast />);

  fireEvent.click(document.querySelector('[aria-label="Dismiss update notice"]') as HTMLElement);

  expect(document.getElementById("update-toast")).toBeNull();
});

test("dismisses automatically after its display interval", () => {
  vi.useFakeTimers();
  render(<UpdateToast />);

  act(() => vi.advanceTimersByTime(UPDATE_TOAST_DURATION_MS));

  expect(document.getElementById("update-toast")).toBeNull();
});

import { render, screen } from "@testing-library/react";
import { afterEach } from "vitest";
import { App } from "./App.tsx";

afterEach(() => {
  document.body.innerHTML = "";
});

test("renders the Codex Voice heading", () => {
  render(<App />);
  expect(screen.getByRole("heading", { name: "Codex Voice" })).toBeDefined();
});

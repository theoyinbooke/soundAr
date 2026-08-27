import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AssistantLauncher, AssistantPane } from "./AssistantPane";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
afterEach(cleanup);

describe("AssistantPane", () => {
  it("opens from a restrained floating action", async () => {
    const onClick = vi.fn();
    render(<AssistantLauncher onClick={onClick} />);
    await userEvent.click(screen.getByRole("button", { name: "Open soundAr assistant" }));
    expect(onClick).toHaveBeenCalledOnce();
  });

  it("loads Codex controls and completes the preview conversation flow", async () => {
    render(<AssistantPane open onClose={vi.fn()} />);
    expect(await screen.findByRole("complementary", { name: "soundAr assistant" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /GPT-5.6-Sol/i })).toBeInTheDocument();
    const composer = screen.getByRole("textbox", { name: "Message soundAr assistant" });
    await userEvent.type(composer, "Create a short ambient cue{enter}");
    expect(screen.getByText("Create a short ambient cue")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText("Get studio state")).toBeInTheDocument(), { timeout: 2_000 });
    expect(screen.getByText(/working brief/i)).toBeInTheDocument();
    expect(screen.getByRole("list", { name: "Current plan" })).toBeInTheDocument();
    expect(screen.getByText("Queue music generation")).toBeInTheDocument();
  });

  it("keeps full access explicit and user selectable", async () => {
    render(<AssistantPane open onClose={vi.fn()} />);
    await screen.findByRole("button", { name: /Studio access/i });
    await userEvent.click(screen.getByRole("button", { name: /Studio access/i }));
    await userEvent.click(screen.getByRole("menuitemradio", { name: /Full access/i }));
    expect(screen.getByRole("button", { name: /Full access/i })).toBeInTheDocument();
  });
});

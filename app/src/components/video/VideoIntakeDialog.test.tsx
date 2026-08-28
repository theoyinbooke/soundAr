import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { VideoIntakeDialog } from "./VideoIntakeDialog";

const tools = [{ id: "ffmpeg" as const, label: "FFmpeg", state: "ready" as const }];

function props(overrides: Partial<React.ComponentProps<typeof VideoIntakeDialog>> = {}): React.ComponentProps<typeof VideoIntakeDialog> {
  return {
    entry: "link",
    tools,
    onClose: vi.fn(),
    onPreviewLink: vi.fn(async (exactUrl: string) => ({ exact_url: exactUrl, title: "Authorized source", creator: "Owner", duration_ms: 30_000, published_label: "Today", preview_url: "data:video/mp4;base64,AAAA", is_single_source: true })),
    onPickLocalVideo: vi.fn(async () => undefined),
    onPickLocalAudio: vi.fn(async () => undefined),
    onImportLink: vi.fn(async () => undefined),
    onImportLocalVideo: vi.fn(async () => undefined),
    onCreateVideo: vi.fn(async () => undefined),
    ...overrides,
  };
}

afterEach(cleanup);

describe("VideoIntakeDialog", () => {
  it("resets rights whenever the exact URL changes", async () => {
    const user = userEvent.setup();
    const onPreviewLink = vi.fn(async (exactUrl: string) => ({ exact_url: exactUrl, title: "Authorized source", creator: "Owner", duration_ms: 30_000, published_label: "Today", preview_url: "data:video/mp4;base64,AAAA", is_single_source: true }));
    render(<VideoIntakeDialog {...props({ onPreviewLink })} />);

    const input = screen.getByRole("textbox", { name: "Video URL" });
    const rights = screen.getByRole("checkbox", { name: /rights or permission.*exact URL/i });
    await user.type(input, "https://example.com/video/one");
    expect(await screen.findByText("Authorized source")).toBeVisible();
    await user.click(rights);
    expect(rights).toBeChecked();

    await user.clear(input);
    await user.type(input, "https://example.com/video/two");
    expect(rights).not.toBeChecked();
    await waitFor(() => expect(onPreviewLink).toHaveBeenLastCalledWith("https://example.com/video/two"));
  });

  it("closes on Escape and restores focus to the launcher", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const launcher = document.createElement("button");
    launcher.textContent = "Launch intake";
    document.body.appendChild(launcher);
    launcher.focus();
    const { unmount } = render(<VideoIntakeDialog {...props({ onClose })} />);
    expect(await screen.findByRole("textbox", { name: "Video URL" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(onClose).toHaveBeenCalledOnce();
    unmount();
    expect(launcher).toHaveFocus();
    launcher.remove();
  });

  it("requires a fresh rights confirmation when the local video changes", async () => {
    const user = userEvent.setup();
    const onPickLocalVideo = vi.fn()
      .mockResolvedValueOnce({ local_path: "/media/first.mp4", display_name: "first.mp4" })
      .mockResolvedValueOnce({ local_path: "/media/second.mp4", display_name: "second.mp4" });
    render(<VideoIntakeDialog {...props({ entry: "upload", onPickLocalVideo })} />);

    const chooser = screen.getByRole("button", { name: "Choose a local video" });
    await user.click(chooser);
    expect(await screen.findByText("first.mp4")).toBeVisible();
    const rights = screen.getByRole("checkbox", { name: /own this media or have permission/i });
    await user.click(rights);
    expect(rights).toBeChecked();

    await user.click(chooser);
    expect(await screen.findByText("second.mp4")).toBeVisible();
    expect(rights).not.toBeChecked();
  });
});

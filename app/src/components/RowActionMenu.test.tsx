import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RowActionMenu } from "./ui";

afterEach(cleanup);

describe("RowActionMenu", () => {
  it("supports keyboard navigation, action selection, and focus restoration", async () => {
    const user = userEvent.setup();
    const choose = vi.fn();
    render(<RowActionMenu label="More actions for item" actions={[
      { label: "First action", onSelect: vi.fn() },
      { label: "Unavailable action", disabled: true, onSelect: vi.fn() },
      { label: "Delete item", danger: true, onSelect: choose },
    ]} />);

    const trigger = screen.getByRole("button", { name: "More actions for item" });
    await user.click(trigger);
    await waitFor(() => expect(screen.getByRole("menuitem", { name: "First action" })).toHaveFocus());
    fireEvent.keyDown(screen.getByRole("menu"), { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "Delete item" })).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(choose).toHaveBeenCalledOnce();
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();

    await user.click(trigger);
    await user.keyboard("{Escape}");
    expect(trigger).toHaveFocus();
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("dismisses on an outside pointer and keeps disabled actions inert", async () => {
    const user = userEvent.setup();
    const action = vi.fn();
    render(<><RowActionMenu label="More actions" actions={[{ label: "Disabled", disabled: true, onSelect: action }]} /><button type="button">Outside</button></>);
    await user.click(screen.getByRole("button", { name: "More actions" }));
    expect(screen.getByRole("menuitem", { name: "Disabled" })).toBeDisabled();
    fireEvent.pointerDown(screen.getByRole("button", { name: "Outside" }));
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(action).not.toHaveBeenCalled();
  });
});

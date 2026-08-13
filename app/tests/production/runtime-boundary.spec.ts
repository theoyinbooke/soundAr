import { expect, test } from "@playwright/test";

test("production fails closed when the native desktop runtime is absent", async ({ page }) => {
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Local runtime unavailable" })).toBeVisible();
  await expect(page.getByRole("alert")).toContainText("No preview data was loaded");
  await expect(page.getByRole("button", { name: "Retry" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Generate" })).toHaveCount(0);
  await expect(page.getByText("Mara", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Browser preview", { exact: true })).toHaveCount(0);
});

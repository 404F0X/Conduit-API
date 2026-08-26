import { PromptsActionDialog } from './prompts-action-dialog';
import { PromptsBulkDeleteDialog } from './prompts-bulk-delete-dialog';
import { PromptsBulkDisableDialog } from './prompts-bulk-disable-dialog';
import { PromptsBulkEnableDialog } from './prompts-bulk-enable-dialog';
import { PromptsDeleteDialog } from './prompts-delete-dialog';

export function PromptsDialogs() {
  return (
    <>
      <PromptsActionDialog />
      <PromptsDeleteDialog />
      <PromptsBulkEnableDialog />
      <PromptsBulkDisableDialog />
      <PromptsBulkDeleteDialog />
    </>
  );
}

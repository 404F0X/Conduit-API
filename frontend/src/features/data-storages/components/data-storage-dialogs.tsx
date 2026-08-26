'use client';

import { ArchiveDataStorageDialog } from './archive-data-storage-dialog';
import { CreateDataStorageDialog } from './create-data-storage-dialog';
import { EditDataStorageDialog } from './edit-data-storage-dialog';

export function DataStorageDialogs() {
  return (
    <>
      <CreateDataStorageDialog />
      <EditDataStorageDialog />
      <ArchiveDataStorageDialog />
    </>
  );
}

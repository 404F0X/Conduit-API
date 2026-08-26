import { create } from 'zustand';

const PROJECT_STORAGE_KEY = 'conduit_selected_project_id';

interface ProjectState {
  selectedProjectId: string | null;
  setSelectedProjectId: (projectId: string | null) => void;
  clearSelectedProjectId: () => void;
}

// Helper functions for localStorage
export const getProjectIdFromStorage = (): string | null => {
  try {
    return localStorage.getItem(PROJECT_STORAGE_KEY);
  } catch {
    return null;
  }
};

const setProjectIdToStorage = (projectId: string): void => {
  try {
    localStorage.setItem(PROJECT_STORAGE_KEY, projectId);
  } catch {
    // Storage can be unavailable in privacy-restricted browser contexts.
  }
};

const removeProjectIdFromStorage = (): void => {
  try {
    localStorage.removeItem(PROJECT_STORAGE_KEY);
  } catch {
    // Storage can be unavailable in privacy-restricted browser contexts.
  }
};

export const useProjectStore = create<ProjectState>()((set) => {
  const initProjectId = getProjectIdFromStorage();

  return {
    selectedProjectId: initProjectId,
    setSelectedProjectId: (projectId) => {
      set({ selectedProjectId: projectId });
      if (projectId) {
        setProjectIdToStorage(projectId);
      } else {
        removeProjectIdFromStorage();
      }
    },
    clearSelectedProjectId: () => {
      set({ selectedProjectId: null });
      removeProjectIdFromStorage();
    },
  };
});

// Convenience hook to get the selected project ID
export const useSelectedProjectId = () => useProjectStore((state) => state.selectedProjectId);

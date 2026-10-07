import { create } from 'zustand';
import { getTokenFromStorage, setTokenToStorage, removeTokenFromStorage, getUserFromStorage, setUserToStorage } from './auth-storage';

export { ACCESS_TOKEN, getTokenFromStorage, setTokenToStorage, removeTokenFromStorage } from './auth-storage';

interface Role {
  code: string;
  name: string;
}

interface Project {
  projectID: string;
  isOwner: boolean;
  scopes: string[];
  roles: Role[];
}

export interface AuthUser {
  id: string;
  email: string;
  firstName: string;
  lastName: string;
  isOwner: boolean;
  preferLanguage: string;
  avatar?: string;
  scopes: string[];
  roles: Role[];
  projects: Project[];
  oidcIdentities?: { id: string; idpName: string; issuer: string; subject: string; email: string }[];
  hasPassword?: boolean;
}

interface AuthState {
  auth: {
    user: AuthUser | null;
    setUser: (user: AuthUser | null) => void;
    accessToken: string;
    setAccessToken: (accessToken: string, persistent?: boolean) => void;
    resetAccessToken: () => void;
    reset: () => void;
  };
}

export const useAuthStore = create<AuthState>()((set) => {
  const initToken = getTokenFromStorage();
  const initUser = getUserFromStorage<AuthUser>();

  return {
    auth: {
      user: initUser,
      setUser: (user) =>
        set((state) => {
          setUserToStorage(user);
          return { ...state, auth: { ...state.auth, user } };
        }),
      accessToken: initToken,
      setAccessToken: (accessToken, persistent = true) =>
        set((state) => {
          setTokenToStorage(accessToken, persistent);
          return { ...state, auth: { ...state.auth, accessToken } };
        }),
      resetAccessToken: () =>
        set((state) => {
          removeTokenFromStorage();
          return { ...state, auth: { ...state.auth, accessToken: '', user: null } };
        }),
      reset: () =>
        set((state) => {
          removeTokenFromStorage();
          return {
            ...state,
            auth: { ...state.auth, user: null, accessToken: '' },
          };
        }),
    },
  };
});

// export const useAuth = () => useAuthStore((state) => state.auth)

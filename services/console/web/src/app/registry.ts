import type { FeatureModule } from "@/app/types";
import admin from "@/features/admin";
import authPages from "@/features/auth-pages";
import automation from "@/features/automation";
import credentials from "@/features/credentials";
import oauthApps from "@/features/oauth-apps";
import principals from "@/features/principals";
import roles from "@/features/roles";
import secrets from "@/features/secrets";
import skills from "@/features/skills";

export const features: FeatureModule[] = [
  authPages,
  principals,
  roles,
  secrets,
  credentials,
  oauthApps,
  admin,
  automation,
  skills,
];

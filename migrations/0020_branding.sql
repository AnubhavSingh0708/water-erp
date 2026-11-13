-- Company branding: Neersagar. Only replaces the untouched factory defaults (never overwrites a value an Admin has set).
UPDATE system_settings SET value = '"Neersagar"' WHERE key = 'plant.name' AND value = '"Aqua Pure Plant"';
UPDATE system_settings SET value = '"NSG"'       WHERE key = 'plant.code' AND value = '"PLT1"';

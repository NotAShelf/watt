// Try /sys/devices/platform paths for thermal zones as a last resort
// if temperature_celsius.is_none() {
//     if let Ok(thermal_zones) = fs::read_dir("/sys/devices/virtual/thermal") {
//         for entry in thermal_zones.flatten() {
//             let zone_path = entry.path();
//             let name = entry.file_name().into_string().unwrap_or_default();

//             if name.starts_with("thermal_zone") {
//                 // Try to match by type
//                 if let Ok(zone_type) = read_sysfs_file_trimmed(zone_path.join("type")) {
//                     if zone_type.contains("cpu")
//                         || zone_type.contains("x86")
//                         || zone_type.contains("core")
//                     {
//                         if let Ok(temp_mc) = read_sysfs_value::<i32>(zone_path.join("temp")) {
//                             temperature_celsius = Some(temp_mc as f32 / 1000.0);
//                             break;
//                         }
//                     }
//                 }
//             }
//         }
//     }
// }

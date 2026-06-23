//! VERSION() / ASTERISKVERSION() functions.
//!
//! Port of func_version.c from Asterisk C.
//!
//! Provides:
//! - VERSION([info]) - return Rustisk version info
//!
//! Info fields:
//! - (empty) - full version string (e.g., "21.0.0")
//! - ASTERISK_VERSION_NUM - numeric version (e.g., "210000")
//! - BUILD_USER - build user name
//! - BUILD_HOSTNAME - build host name
//! - BUILD_MACHINE - build machine type
//! - BUILD_OS - build OS
//! - BUILD_DATE - build date
//! - BUILD_KERNEL - build kernel version

use crate::{DialplanFunc, FuncContext, FuncError, FuncResult};

/// The version string for this Rustisk build.
pub const RUSTISK_VERSION: &str = "Rustisk 0.1.0";
/// Numeric version (21.0.0 -> 210000).
pub const RUSTISK_VERSION_NUM: &str = "100";

/// VERSION() function.
pub struct FuncVersion;

impl DialplanFunc for FuncVersion {
    fn name(&self) -> &str {
        "VERSION"
    }

    fn read(&self, _ctx: &FuncContext, args: &str) -> FuncResult {
        let field = args.trim().to_uppercase();
        match field.as_str() {
            "" => Ok(RUSTISK_VERSION.to_string()),
            "ASTERISK_VERSION_NUM" => Ok(RUSTISK_VERSION_NUM.to_string()),
            "BUILD_USER" => Ok(std::env::var("USER").unwrap_or_else(|_| "unknown".into())),
            "BUILD_HOSTNAME" => Ok("localhost".to_string()),
            "BUILD_MACHINE" => Ok(std::env::consts::ARCH.to_string()),
            "BUILD_OS" => Ok(std::env::consts::OS.to_string()),
            "BUILD_DATE" => Ok("2025-01-01".to_string()),
            "BUILD_KERNEL" => Ok(std::env::consts::OS.to_string()),
            _ => Err(FuncError::InvalidArgument(format!(
                "VERSION: unknown info field '{}'", field
            ))),
        }
    }
}

/// ASTERISKVERSION() function - alias for VERSION().
pub struct FuncAsteriskVersion;

impl DialplanFunc for FuncAsteriskVersion {
    fn name(&self) -> &str {
        "ASTERISKVERSION"
    }

    fn read(&self, ctx: &FuncContext, args: &str) -> FuncResult {
        FuncVersion.read(ctx, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_default() {
        let ctx = FuncContext::new();
        let func = FuncVersion;
        let result = func.read(&ctx, "").unwrap();
        assert_eq!(result, RUSTISK_VERSION);
    }

    #[test]
    fn test_version_num() {
        let ctx = FuncContext::new();
        let func = FuncVersion;
        let result = func.read(&ctx, "ASTERISK_VERSION_NUM").unwrap();
        assert_eq!(result, RUSTISK_VERSION_NUM);
    }

    #[test]
    fn test_version_build_os() {
        let ctx = FuncContext::new();
        let func = FuncVersion;
        let result = func.read(&ctx, "BUILD_OS").unwrap();
        assert!(!result.is_empty());
    }

    #[test]
    fn test_version_invalid() {
        let ctx = FuncContext::new();
        let func = FuncVersion;
        assert!(func.read(&ctx, "INVALID").is_err());
    }

    #[test]
    fn test_asterisk_version_alias() {
        let ctx = FuncContext::new();
        let func = FuncAsteriskVersion;
        let result = func.read(&ctx, "").unwrap();
        assert_eq!(result, RUSTISK_VERSION);
    }
}

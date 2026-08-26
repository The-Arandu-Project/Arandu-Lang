//! Typed identities shared by package discovery, module mapping and queries.

macro_rules! package_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u32);

        impl $name {
            pub fn try_from_usize(value: usize) -> Option<Self> {
                u32::try_from(value).ok().map(Self)
            }

            #[must_use]
            pub const fn as_u32(self) -> u32 {
                self.0
            }
        }
    };
}

package_id!(PackageId);
package_id!(TargetId);
package_id!(ModuleId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_id_conversions_are_checked_and_types_do_not_mix() {
        assert_eq!(PackageId::try_from_usize(7).unwrap().as_u32(), 7);
        assert_eq!(TargetId::try_from_usize(7).unwrap().as_u32(), 7);
        assert_eq!(ModuleId::try_from_usize(7).unwrap().as_u32(), 7);
        if usize::BITS > u32::BITS {
            assert!(PackageId::try_from_usize((u32::MAX as usize) + 1).is_none());
        }
    }
}

/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIImagePickerController`

use crate::frameworks::foundation::NSInteger;
use crate::objc::{id, nil, objc_classes, release, ClassExports};

type UIImagePickerControllerSourceType = NSInteger;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// TODO: should extend UINavigationController, which extends
//       UIViewController.
@implementation UIImagePickerController: NSObject

+ (bool)isSourceTypeAvailable:(UIImagePickerControllerSourceType)_type {
    // For now, simply claim no sources are available.
    // TODO: support some sources.
    false
}

- (id)init {
    release(env, this);
    nil // FIXME: real implementation
}

@end

};

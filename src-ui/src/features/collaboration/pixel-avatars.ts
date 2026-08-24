export type PixelHairStyle = 'crop' | 'curls' | 'mohawk' | 'long' | 'cap' | 'bob' | 'bun';
export type PixelAccessory = 'none' | 'overall' | 'scarf' | 'tie' | 'headphones' | 'glasses' | 'vest';

export interface PixelAvatarProfile {
  id: string;
  name: string;
  hairStyle: PixelHairStyle;
  accessory: PixelAccessory;
  skin: string;
  hair: string;
  top: string;
  trim: string;
  pants: string;
  shoes: string;
}

// Original Teak characters for the top-down office roster. Every profile
// changes hair/clothing geometry, not just the palette, so seats stay
// legible at 2x. These are not copies of any third-party sprite sheet.
export const PIXEL_AVATARS: PixelAvatarProfile[] = [
  { id: 'cedar', name: 'Cedar', hairStyle: 'crop', accessory: 'none', skin: '#dca779', hair: '#3a2520', top: '#a65f42', trim: '#e0ad68', pants: '#39475d', shoes: '#232735' },
  { id: 'moss', name: 'Moss', hairStyle: 'curls', accessory: 'overall', skin: '#7c4f3a', hair: '#201a18', top: '#5f8c67', trim: '#d3b769', pants: '#354f47', shoes: '#1e2925' },
  { id: 'ember', name: 'Ember', hairStyle: 'mohawk', accessory: 'scarf', skin: '#c9825d', hair: '#9f3f35', top: '#4d596c', trim: '#e08a4f', pants: '#30384a', shoes: '#20242d' },
  { id: 'luna', name: 'Luna', hairStyle: 'long', accessory: 'tie', skin: '#f0c39c', hair: '#2e253c', top: '#6578a6', trim: '#d6b8dd', pants: '#343c5b', shoes: '#24283b' },
  { id: 'tide', name: 'Tide', hairStyle: 'cap', accessory: 'headphones', skin: '#b97455', hair: '#2b2525', top: '#328d94', trim: '#8fd5cc', pants: '#2f4c5a', shoes: '#203038' },
  { id: 'plum', name: 'Plum', hairStyle: 'bob', accessory: 'glasses', skin: '#e2a985', hair: '#572f55', top: '#865b83', trim: '#e4afce', pants: '#483d5f', shoes: '#2b2637' },
  { id: 'honey', name: 'Honey', hairStyle: 'bun', accessory: 'vest', skin: '#9c684c', hair: '#5a3823', top: '#d39a45', trim: '#f0cf7a', pants: '#51463d', shoes: '#2e2926' },
];

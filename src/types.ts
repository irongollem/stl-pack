export type StlModel = {
  modelName: string;
  description: string;
  release?: string;
  releaseDate?: string; // MM/YYYY
  designer?: string;
  tags: string[];
  pictures: Picture[];
  modelFiles: FileMeta[];
};

export type Picture = {
  name: string;
  path: string;
  file: File;
};

export type FileMeta = {
  name: string;
  path: string;
  file: File;
};

export const fileLogos = {
  chitubox: '/chitubox.jpg',
  lychee: '/lychee.jpg',
  stl: '/stl.jpg',
}
